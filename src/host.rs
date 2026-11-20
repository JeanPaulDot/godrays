//! Hosting: running the Kyber server (kycontroller) from the launcher.
//!
//! The launcher does not use the Windows service the Kyber installer sets
//! up. It starts kycontroller as an ordinary process with a configuration
//! file of its own, so starting, stopping and changing the sign-in never
//! need administrator rights. The trade-off, per Kyber's documentation: a
//! server started this way cannot show Windows permission prompts or the
//! lock screen to the remote side.

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::{Config, Encoder, Hosting};
use crate::kyber::{server_binary, sha256_hex};
use crate::kyclient::{hide_console, log_tail, spawn_log_reader, LogBuffer};
use crate::logtail::LogTail;
use crate::winproc::{boost_process, JobGuard, ShutdownEvent};

/// Kyber stores basic-auth passwords as the hex SHA-256 of the password.
pub fn password_hash(password: &str) -> String {
    sha256_hex(password.as_bytes())
}

fn table<'a>(parent: &'a mut toml::Table, key: &str) -> &'a mut toml::Table {
    let entry = parent
        .entry(key.to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !entry.is_table() {
        *entry = toml::Value::Table(toml::Table::new());
    }
    entry.as_table_mut().expect("just made a table")
}

/// Builds the server configuration: the installed one (which carries the
/// encoder the installer detected) with the launcher's port and sign-in.
/// `certificate` is the (certificate, key) pair to serve, when known.
pub fn build_server_config(
    template: &str,
    hosting: &Hosting,
    password: &str,
    certificate: Option<(&Path, &Path)>,
) -> Result<String, String> {
    let mut root: toml::Table = template
        .parse()
        .map_err(|e| format!("Kyber's configuration file could not be read: {e}"))?;

    let controller = table(&mut root, "kycontroller");
    controller.insert("port".into(), toml::Value::Integer(hosting.port.into()));
    if let Some((cert, key)) = certificate {
        // Forward slashes: a backslash is an escape character in TOML.
        let as_text =
            |path: &Path| toml::Value::String(path.display().to_string().replace('\\', "/"));
        controller.insert("tls_cert".into(), as_text(cert));
        controller.insert("tls_key".into(), as_text(key));
    }

    let auth = table(controller, "auth");
    let basic = table(auth, "basic");
    basic.insert("enabled".into(), toml::Value::Boolean(true));
    let mut login = toml::Table::new();
    login.insert(
        "username".into(),
        toml::Value::String(hosting.username.clone()),
    );
    login.insert(
        "password".into(),
        toml::Value::String(password_hash(password)),
    );
    basic.insert(
        "logins".into(),
        toml::Value::Array(vec![toml::Value::Table(login)]),
    );
    // Kyber's template enables token sign-in with a published development
    // key. Never host with that.
    table(auth, "jwt").insert("enabled".into(), toml::Value::Boolean(false));

    if hosting.engine != crate::config::Engine::Default {
        table(&mut root, "kyavserver").insert(
            "backend".into(),
            toml::Value::String(hosting.engine.flag().to_owned()),
        );
    }
    if hosting.encoder != Encoder::Auto {
        table(&mut root, "kyavserver").insert(
            "encoder".into(),
            toml::Value::String(hosting.encoder.flag().to_owned()),
        );
    }
    toml::to_string(&root).map_err(|e| e.to_string())
}

pub struct ServerFiles {
    pub config: PathBuf,
    pub log_dir: PathBuf,
}

/// Writes the launcher's server configuration next to its own settings.
pub fn prepare(
    install_dir: &Path,
    hosting: &Hosting,
    password: &str,
) -> Result<ServerFiles, String> {
    prepare_in(&Config::dir().join("host"), install_dir, hosting, password)
}

/// Like [`prepare`], writing into `dir`.
pub fn prepare_in(
    dir: &Path,
    install_dir: &Path,
    hosting: &Hosting,
    password: &str,
) -> Result<ServerFiles, String> {
    if hosting.username.trim().is_empty() || password.is_empty() {
        return Err("Set a hosting name and password first.".to_owned());
    }
    let template =
        std::fs::read_to_string(install_dir.join("kyber_config.toml")).unwrap_or_default();
    let cert = install_dir.join("kybertest_cert.pem");
    let key = install_dir.join("kybertest_key.pem");
    let certificate = (cert.is_file() && key.is_file()).then_some((cert.as_path(), key.as_path()));
    let text = build_server_config(&template, hosting, password, certificate)?;

    let log_dir = dir.join("log");
    std::fs::create_dir_all(&log_dir).map_err(|e| e.to_string())?;
    let config = dir.join("kyber_config.toml");
    std::fs::write(&config, text).map_err(|e| e.to_string())?;
    Ok(ServerFiles { config, log_dir })
}

/// True when something on this PC already uses `port`, for TCP (Kyber's
/// HTTPS) or UDP (its QUIC stream): a UDP clash makes clients connect and
/// then wait forever for the picture.
pub fn port_in_use(port: u16) -> bool {
    let local = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&local, Duration::from_millis(300)).is_ok()
        || std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).is_err()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerState {
    /// The process is up but not accepting connections yet.
    Starting,
    Ready,
    Exited(Option<i32>),
}

/// How long the server gets to end its sessions and exit by itself before
/// it is terminated.
const GRACEFUL_STOP: Duration = Duration::from_secs(4);

struct Shared {
    state: Mutex<ServerState>,
    log: LogBuffer,
    stop: AtomicBool,
    /// Ids of the client sessions currently connected.
    clients: Mutex<HashSet<String>>,
    boost: AtomicBool,
    /// Processes boosted so far: (CPU, GPU).
    boosted: Mutex<(usize, usize)>,
}

/// Keeps `clients` in step with one line of the server's log.
fn track_clients(line: &str, clients: &mut HashSet<String>) {
    if line.contains("controlplane connected") {
        if let Some(id) = line.split('"').nth(1) {
            clients.insert(id.to_owned());
        }
    } else if let Some((_, rest)) = line.split_once("Control Websocket disconnected for session ") {
        clients.remove(rest.trim());
    }
}

/// A Kyber server started by the launcher.
pub struct Server {
    shared: Arc<Shared>,
    pub started: Instant,
}

impl Server {
    /// `boost`: put the server's processes ahead of games (see
    /// `winproc::boost_process`).
    pub fn start(
        install_dir: &Path,
        files: &ServerFiles,
        port: u16,
        boost: bool,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let server = Self::start_binary(
            &server_binary(install_dir),
            install_dir,
            files,
            port,
            &[],
            wake,
        )?;
        server.shared.boost.store(boost, Ordering::SeqCst);
        Ok(server)
    }

    /// How many server processes run with raised CPU and GPU priority.
    pub fn boosted(&self) -> (usize, usize) {
        *self.shared.boosted.lock().unwrap()
    }

    fn start_binary(
        binary: &Path,
        working_dir: &Path,
        files: &ServerFiles,
        port: u16,
        extra_env: &[(&str, &str)],
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        if port_in_use(port) {
            return Err(format!(
                "Port {port} is already in use on this PC, perhaps by the Kyber Windows service or another program. Stop it or pick another port."
            ));
        }
        let shutdown = ShutdownEvent::new();
        let mut command = Command::new(binary);
        command
            .arg("--config")
            .arg(&files.config)
            // The default log folder belongs to the Windows service.
            .env("KYBER_LOG_DIR", &files.log_dir)
            .envs(extra_env.iter().copied())
            .current_dir(working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(event) = &shutdown {
            command.env("KYBER_SHUTDOWN_EVENT", event.env_value());
        }
        hide_console(&mut command);
        let mut tail = LogTail::from_end(files.log_dir.join("kycontroller.log"));
        let mut child = command
            .spawn()
            .map_err(|e| format!("Could not start {}: {e}", binary.display()))?;
        // The server starts helper processes; the job ends them with it.
        let job = JobGuard::attach(&child);

        let shared = Arc::new(Shared {
            state: Mutex::new(ServerState::Starting),
            log: LogBuffer::default(),
            stop: AtomicBool::new(false),
            clients: Mutex::new(HashSet::new()),
            boost: AtomicBool::new(false),
            boosted: Mutex::new((0, 0)),
        });
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        if let Some(out) = child.stdout.take() {
            spawn_log_reader(out, shared.log.clone(), wake.clone());
        }
        if let Some(err) = child.stderr.take() {
            spawn_log_reader(err, shared.log.clone(), wake.clone());
        }

        let watcher = shared.clone();
        std::thread::spawn(move || {
            let mut job = job;
            let mut boosted_pids: HashSet<u32> = HashSet::new();
            let mut next_boost = Instant::now();
            let mut ready = false;
            let mut next_probe = Instant::now() + Duration::from_millis(300);
            let mut next_log_check = Instant::now();
            let mut follow_log = |watcher: &Shared| {
                let lines = tail.poll();
                if lines.is_empty() {
                    return false;
                }
                let mut clients = watcher.clients.lock().unwrap();
                let mut log = watcher.log.lock().unwrap();
                for line in lines {
                    track_clients(&line, &mut clients);
                    if log.len() == 400 {
                        log.pop_front();
                    }
                    log.push_back(line);
                }
                true
            };
            let code = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break status.code(),
                    Ok(None) => {}
                    Err(_) => break None,
                }
                if watcher.stop.load(Ordering::SeqCst) {
                    // First ask nicely: the server then ends its client
                    // sessions and unplugs its virtual gamepads itself.
                    let mut exited = None;
                    if let Some(event) = &shutdown {
                        event.signal();
                        let deadline = Instant::now() + GRACEFUL_STOP;
                        while Instant::now() < deadline {
                            if let Ok(Some(status)) = child.try_wait() {
                                exited = Some(status.code());
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(25));
                        }
                    }
                    // Then make sure nothing is left: closing the job ends
                    // the helpers, and the server if it is still there.
                    job.take();
                    break match exited {
                        Some(code) => code,
                        None => {
                            let _ = child.kill();
                            child.wait().ok().and_then(|s| s.code())
                        }
                    };
                }
                if !ready && Instant::now() >= next_probe {
                    next_probe = Instant::now() + Duration::from_millis(500);
                    if port_in_use(port) {
                        ready = true;
                        *watcher.state.lock().unwrap() = ServerState::Ready;
                        wake();
                    }
                }
                // The encoder process starts with each session: keep looking.
                if watcher.boost.load(Ordering::SeqCst) && Instant::now() >= next_boost {
                    next_boost = Instant::now() + Duration::from_millis(500);
                    if let Some(job) = &job {
                        for pid in job.process_ids() {
                            if boosted_pids.insert(pid) {
                                let (cpu, gpu) = boost_process(pid);
                                let mut count = watcher.boosted.lock().unwrap();
                                count.0 += usize::from(cpu);
                                count.1 += usize::from(gpu);
                            }
                        }
                    }
                }
                if Instant::now() >= next_log_check {
                    next_log_check = Instant::now() + Duration::from_millis(300);
                    if follow_log(&watcher) {
                        wake();
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            job.take();
            follow_log(&watcher);
            watcher.clients.lock().unwrap().clear();
            *watcher.state.lock().unwrap() = ServerState::Exited(code);
            wake();
        });

        Ok(Self {
            shared,
            started: Instant::now(),
        })
    }

    pub fn state(&self) -> ServerState {
        self.shared.state.lock().unwrap().clone()
    }

    /// Number of clients connected right now.
    pub fn clients(&self) -> usize {
        self.shared.clients.lock().unwrap().len()
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }

    /// True once the launcher asked the server to stop, as opposed to the
    /// server ending on its own.
    pub fn stop_requested(&self) -> bool {
        self.shared.stop.load(Ordering::SeqCst)
    }

    pub fn log_tail(&self, lines: usize) -> Vec<String> {
        log_tail(&self.shared.log, lines)
    }

    /// Stops the server and waits until it is gone.
    pub fn stop_and_wait(&self) {
        self.stop();
        let deadline = Instant::now() + GRACEFUL_STOP + Duration::from_secs(2);
        while !matches!(self.state(), ServerState::Exited(_)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

// ------------------------------------------------------------ encoder choice

/// Encoders worth trying on a PC with these graphics cards, best first.
/// Hardware encoders are much faster than software; x264 works everywhere
/// and is always the last resort.
pub fn encoders_for_gpus(names: &[String]) -> Vec<Encoder> {
    let mut encoders = Vec::new();
    let mut add = |encoder| {
        if !encoders.contains(&encoder) {
            encoders.push(encoder);
        }
    };
    for name in names {
        let name = name.to_lowercase();
        if name.contains("nvidia") || name.contains("geforce") || name.contains("quadro") {
            add(Encoder::Nvenc);
        } else if name.contains("amd") || name.contains("radeon") {
            add(Encoder::Amf);
        } else if name.contains("intel") {
            add(Encoder::Qsv);
        }
    }
    // A dedicated card beats integrated graphics.
    encoders.sort_by_key(|encoder| match encoder {
        Encoder::Nvenc => 0,
        Encoder::Amf => 1,
        _ => 2,
    });
    encoders.push(Encoder::X264);
    encoders
}

/// Names of the graphics cards Windows knows about.
pub fn gpu_names() -> Vec<String> {
    let mut command = Command::new("powershell");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-CimInstance Win32_VideoController).Name",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut command);
    match command.output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = r#"
[kyavserver]
encoder = "nvenc"

[kycontroller]
#port = 8080

[kycontroller.security]
trusted_origins = []
same_site = true

[kycontroller.auth.basic]
enabled = false
logins = [
    { username = "Default user", password = "c2bb5d1edbff196e3a879ec0a59aeb3952822266909b5ba07b45072d20a05446" },
]

[kycontroller.auth.jwt]
enabled = true
"#;

    fn hosting() -> Hosting {
        Hosting {
            username: "kyber".into(),
            port: 9000,
            encoder: Encoder::Auto,
            ..Hosting::default()
        }
    }

    #[test]
    fn passwords_are_hashed_the_way_kyber_expects() {
        // The login shipped in Kyber's own template is "Default password".
        assert_eq!(
            password_hash("Default password"),
            "c2bb5d1edbff196e3a879ec0a59aeb3952822266909b5ba07b45072d20a05446"
        );
    }

    #[test]
    fn server_config_gets_the_launcher_sign_in_and_keeps_the_rest() {
        let cert = Path::new(r"C:\Program Files\Kyber\kybertest_cert.pem");
        let key = Path::new(r"C:\Program Files\Kyber\kybertest_key.pem");
        let text =
            build_server_config(TEMPLATE, &hosting(), "abcd-2345-wxyz", Some((cert, key))).unwrap();
        let config: toml::Table = text.parse().unwrap();

        let controller = config["kycontroller"].as_table().unwrap();
        assert_eq!(controller["port"].as_integer(), Some(9000));
        assert_eq!(
            controller["tls_cert"].as_str(),
            Some("C:/Program Files/Kyber/kybertest_cert.pem")
        );
        let basic = controller["auth"]["basic"].as_table().unwrap();
        assert_eq!(basic["enabled"].as_bool(), Some(true));
        let logins = basic["logins"].as_array().unwrap();
        assert_eq!(logins.len(), 1);
        assert_eq!(logins[0]["username"].as_str(), Some("kyber"));
        assert_eq!(
            logins[0]["password"].as_str(),
            Some(password_hash("abcd-2345-wxyz").as_str())
        );
        // The development-key token sign-in is switched off.
        assert_eq!(controller["auth"]["jwt"]["enabled"].as_bool(), Some(false));
        // Untouched parts survive.
        assert_eq!(config["kyavserver"]["encoder"].as_str(), Some("nvenc"));
        assert_eq!(controller["security"]["same_site"].as_bool(), Some(true));
        // The clear-text password is not written anywhere.
        assert!(!text.contains("abcd-2345-wxyz"));
    }

    #[test]
    fn encoder_choice_and_empty_template_are_handled() {
        let mut h = hosting();
        h.encoder = Encoder::X264;
        let text = build_server_config("", &h, "pw", None).unwrap();
        let config: toml::Table = text.parse().unwrap();
        assert_eq!(config["kyavserver"]["encoder"].as_str(), Some("x264"));
        assert!(config["kycontroller"].get("tls_cert").is_none());
        assert_eq!(
            config["kycontroller"]["auth"]["jwt"]["enabled"].as_bool(),
            Some(false)
        );
        assert!(build_server_config("not [toml", &h, "pw", None).is_err());
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn wait_for(server: &Server, wanted: impl Fn(&ServerState) -> bool) -> ServerState {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = server.state();
            if wanted(&state) {
                return state;
            }
            assert!(Instant::now() < deadline, "server stuck in {state:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn process_alive(pid: &str) -> bool {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).contains(pid)
    }

    /// Runs against the stand-in from `examples/` (see `kyclient.rs`): it
    /// listens like a server and starts a helper process of its own.
    #[cfg(windows)]
    #[test]
    fn a_server_becomes_ready_and_stopping_it_ends_its_helpers_too() {
        let Some(fake) = std::env::var_os("FAKE_KYCLIENT").map(PathBuf::from) else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("godrays-test-host-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files = ServerFiles {
            config: dir.join("kyber_config.toml"),
            log_dir: dir.join("log"),
        };
        let port = free_port();
        let pid_file = dir.join("helper-pid.txt");
        let port_text = port.to_string();
        let env = [
            ("FAKE_SERVER_LISTEN", port_text.as_str()),
            ("FAKE_SERVER_HELPER_PID_FILE", pid_file.to_str().unwrap()),
            ("FAKE_KYCLIENT_SLEEP_MS", "30000"),
        ];

        let server = Server::start_binary(&fake, &dir, &files, port, &env, || {}).unwrap();
        assert_eq!(
            wait_for(&server, |s| *s != ServerState::Starting),
            ServerState::Ready
        );
        let log = server.log_tail(10).join("\n");
        assert!(log.contains("--config"), "log was: {log}");

        // A second server on the same port is refused up front.
        let error = Server::start_binary(&fake, &dir, &files, port, &env, || {})
            .err()
            .unwrap();
        assert!(error.contains("already in use"), "{error}");

        let helper_pid = std::fs::read_to_string(&pid_file).unwrap();
        assert!(process_alive(helper_pid.trim()));
        server.stop();
        wait_for(&server, |s| matches!(s, ServerState::Exited(_)));
        std::thread::sleep(Duration::from_millis(300));
        assert!(!process_alive(helper_pid.trim()), "helper survived");
        assert!(!port_in_use(port));
    }

    #[test]
    fn connected_clients_are_counted_from_the_server_log() {
        let mut clients = HashSet::new();
        let connect =
            |id: &str| format!("INFO kycontroller - Session \"{id}\" controlplane connected");
        track_clients(&connect("aaa"), &mut clients);
        track_clients(&connect("bbb"), &mut clients);
        track_clients(&connect("aaa"), &mut clients);
        assert_eq!(clients.len(), 2);
        track_clients(
            "WARN kycontroller::statemachine - Control Websocket disconnected for session aaa",
            &mut clients,
        );
        assert_eq!(clients.len(), 1);
        assert!(clients.contains("bbb"));
        track_clients("INFO something else entirely", &mut clients);
        assert_eq!(clients.len(), 1);
    }

    #[test]
    fn hardware_encoders_come_first_and_software_is_always_last() {
        let gpus = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(
            encoders_for_gpus(&gpus(&["AMD Radeon RX 5700 XT"])),
            [Encoder::Amf, Encoder::X264]
        );
        // Laptop with integrated Intel graphics and a GeForce: the GeForce wins.
        assert_eq!(
            encoders_for_gpus(&gpus(&["Intel(R) UHD Graphics", "NVIDIA GeForce RTX 4060"])),
            [Encoder::Nvenc, Encoder::Qsv, Encoder::X264]
        );
        assert_eq!(
            encoders_for_gpus(&gpus(&["Microsoft Basic Display Adapter"])),
            [Encoder::X264]
        );
        assert_eq!(encoders_for_gpus(&[]), [Encoder::X264]);
    }

    /// The stand-in honours the shutdown event like the real server does;
    /// this proves the event reaches the child process and is used before
    /// the process is terminated.
    #[cfg(windows)]
    #[test]
    fn stopping_asks_the_server_to_shut_down_before_terminating_it() {
        let Some(fake) = std::env::var_os("FAKE_KYCLIENT").map(PathBuf::from) else {
            return;
        };
        let dir =
            std::env::temp_dir().join(format!("godrays-test-graceful-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files = ServerFiles {
            config: dir.join("kyber_config.toml"),
            log_dir: dir.join("log"),
        };
        let port = free_port();
        let port_text = port.to_string();
        let marker = dir.join("graceful.txt");
        let env = [
            ("FAKE_SERVER_LISTEN", port_text.as_str()),
            ("FAKE_SERVER_GRACEFUL_FILE", marker.to_str().unwrap()),
            ("FAKE_KYCLIENT_SLEEP_MS", "30000"),
        ];
        let server = Server::start_binary(&fake, &dir, &files, port, &env, || {}).unwrap();
        wait_for(&server, |s| *s == ServerState::Ready);
        let stopping = Instant::now();
        server.stop_and_wait();
        assert_eq!(server.state(), ServerState::Exited(Some(0)));
        assert!(
            stopping.elapsed() < Duration::from_secs(2),
            "took the slow path"
        );
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            "shutdown event received"
        );
    }
}
