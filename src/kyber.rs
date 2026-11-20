//! One-click setup of Kyber itself (client and server) on Windows.
//!
//! The launcher downloads the official Kyber installer, checks it against a
//! pinned SHA-256, and runs it silently. Installing needs administrator
//! rights, so that one step runs in a second, elevated copy of the launcher
//! (`--admin-install <task file>`); Windows asks the user for permission once.
//!
//! The Kyber installer registers its server as a Windows service that starts
//! with the PC. The launcher switches that service to manual start: hosting
//! is something the user turns on from the Host screen, not something that
//! happens behind their back.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::kyclient::hide_console;

/// The Kyber release this launcher installs. To move to a newer release,
/// update all four constants from the release's package page on GitLab.
pub const VERSION: &str = "0.29.0";
const INSTALLER_NAME: &str = "kyber-installer-win64-0.29.0.exe";
const INSTALLER_URL: &str = "https://gitlab.com/api/v4/projects/78083474/packages/generic/kyber-installer/0.29.0/kyber-installer-win64-0.29.0.exe";
const INSTALLER_SHA256: &str = "660bd8b22ca12c85034b2e1a538b64ea090c03c80db02592f0ce9291ba56ef29";
pub const INSTALLER_BYTES: u64 = 62_949_240;

const SERVER_BINARY: &str = if cfg!(windows) {
    "kycontroller.exe"
} else {
    "kycontroller"
};

/// Folder of the installed Kyber server, if there is one.
pub fn install_dir(client: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = client.and_then(Path::parent) {
        candidates.push(dir.to_path_buf());
    }
    for var in ["ProgramFiles", "ProgramW6432"] {
        if let Some(base) = std::env::var_os(var) {
            candidates.push(PathBuf::from(base).join("Kyber"));
        }
    }
    candidates
        .into_iter()
        .find(|dir| dir.join(SERVER_BINARY).is_file())
}

pub fn server_binary(install_dir: &Path) -> PathBuf {
    install_dir.join(SERVER_BINARY)
}

/// Whether the ViGEmBus driver, which Kyber needs to create virtual gamepads
/// on a Windows host, is installed.
pub fn vigem_installed() -> bool {
    let mut command = Command::new("sc");
    command
        .args(["query", "ViGEmBus"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut command);
    command.status().map(|s| s.success()).unwrap_or(false)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Downloads `url` to `dest` unless a file with the expected SHA-256 is
/// already there. A download whose hash does not match is deleted.
fn download_verified(
    url: &str,
    dest: &Path,
    expected_sha256: &str,
    progress: &dyn Fn(u64, u64),
) -> Result<(), String> {
    if sha256_file(dest).is_ok_and(|hash| hash == expected_sha256) {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_connect(Some(Duration::from_secs(15)))
        .build()
        .into();
    let response = agent
        .get(url)
        .call()
        .map_err(|e| format!("The download failed: {e}"))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let partial = dest.with_extension("part");
    let mut file = std::fs::File::create(&partial).map_err(|e| e.to_string())?;
    let mut reader = response.into_body().into_reader();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    let mut received = 0u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| format!("The download was interrupted: {e}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).map_err(|e| e.to_string())?;
        hasher.update(&buffer[..read]);
        received += read as u64;
        progress(received, total);
    }
    drop(file);

    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected_sha256 {
        let _ = std::fs::remove_file(&partial);
        return Err(
            "The downloaded installer does not match the expected checksum, so it was discarded."
                .to_owned(),
        );
    }
    std::fs::rename(&partial, dest).map_err(|e| e.to_string())
}

/// Characters the Kyber installer can take on its command line without any
/// quoting. The installer pastes these values into a script, so nothing
/// else is ever passed to it.
pub fn is_safe_credential(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A random password that is easy to read aloud and to type on a gamepad:
/// three groups of four from an alphabet without look-alike characters
/// (about 59 bits).
pub fn generate_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut random = [0u8; 12];
    getrandom::getrandom(&mut random).expect("the OS random source is unavailable");
    let mut password = String::with_capacity(14);
    for (index, byte) in random.iter().enumerate() {
        if index > 0 && index % 4 == 0 {
            password.push('-');
        }
        // 256 is not a multiple of 31, so the first few letters are very
        // slightly more likely; irrelevant at this length.
        password.push(ALPHABET[*byte as usize % ALPHABET.len()] as char);
    }
    password
}

// ------------------------------------------------------- elevated install

/// What the elevated copy of the launcher is asked to do.
#[derive(Serialize, Deserialize)]
struct InstallTask {
    installer: PathBuf,
    username: String,
    password: String,
    /// Where the outcome is written: "ok" or an error message.
    result: PathBuf,
}

/// Entry point of `godrays --admin-install <task file>`. Runs with
/// administrator rights. Returns the process exit code.
pub fn admin_install(task_path: &Path) -> i32 {
    let task: Result<InstallTask, String> = std::fs::read_to_string(task_path)
        .map_err(|e| e.to_string())
        .and_then(|text| toml::from_str(&text).map_err(|e| e.to_string()));
    // The task holds a password: remove it before doing anything else.
    let _ = std::fs::remove_file(task_path);
    let task = match task {
        Ok(task) => task,
        Err(_) => return 2,
    };
    let outcome = run_installer(&task, INSTALLER_SHA256, &[]).and_then(|()| service_to_manual());
    let text = match &outcome {
        Ok(()) => "ok".to_owned(),
        Err(error) => error.clone(),
    };
    let _ = std::fs::write(&task.result, text);
    i32::from(outcome.is_err())
}

/// Runs the Kyber installer silently. The elevated side trusts nothing it
/// was handed: the installer must match the pinned checksum and the
/// credentials must be plain.
fn run_installer(
    task: &InstallTask,
    expected_sha256: &str,
    extra_env: &[(&str, &str)],
) -> Result<(), String> {
    if !is_safe_credential(&task.username) || !is_safe_credential(&task.password) {
        return Err("The hosting name or password contains unsupported characters.".to_owned());
    }
    match sha256_file(&task.installer) {
        Ok(hash) if hash == expected_sha256 => {}
        Ok(_) => return Err("The installer file changed after it was verified.".to_owned()),
        Err(error) => return Err(format!("The installer could not be read: {error}")),
    }
    // /STARTMODE=none: install everything, but do not start hosting now.
    let status = Command::new(&task.installer)
        .arg("/S")
        .arg("/BASIC=1")
        .arg(format!("/BASICUSER={}", task.username))
        .arg(format!("/BASICPASS={}", task.password))
        .arg("/STARTMODE=none")
        .envs(extra_env.iter().copied())
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("The Kyber installer could not be started: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "The Kyber installer ended with code {}.",
            status.code().unwrap_or(-1)
        ))
    }
}

/// Stops the Kyber service from starting (and hosting) at every boot.
fn service_to_manual() -> Result<(), String> {
    let mut command = Command::new("sc");
    command
        .args(["config", "kyber", "start=", "demand"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut command);
    match command.status() {
        Ok(status) if status.success() => Ok(()),
        _ => Err(
            "Kyber was installed, but its Windows service could not be set to manual start."
                .to_owned(),
        ),
    }
}

// ------------------------------------------------------------------ setup

#[derive(Clone, Debug, PartialEq)]
pub enum SetupStage {
    Downloading {
        received: u64,
        total: u64,
    },
    /// Waiting for the Windows permission prompt and the installer.
    Installing,
    Done,
    Failed(String),
}

/// A setup running in the background.
pub struct Setup {
    stage: Arc<Mutex<SetupStage>>,
}

impl Setup {
    /// Starts downloading and installing Kyber. `username` and `password`
    /// become the sign-in of the freshly installed server.
    pub fn start(
        username: String,
        password: String,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let stage = Arc::new(Mutex::new(SetupStage::Downloading {
            received: 0,
            total: INSTALLER_BYTES,
        }));
        let shared = stage.clone();
        std::thread::spawn(move || {
            let set = |next: SetupStage| {
                *shared.lock().unwrap() = next;
                wake();
            };
            let outcome = run_setup(
                &username,
                &password,
                &|received, total| {
                    set(SetupStage::Downloading {
                        received,
                        total: if total > 0 { total } else { INSTALLER_BYTES },
                    })
                },
                &|| set(SetupStage::Installing),
            );
            set(match outcome {
                Ok(()) => SetupStage::Done,
                Err(error) => SetupStage::Failed(error),
            });
        });
        Self { stage }
    }

    pub fn stage(&self) -> SetupStage {
        self.stage.lock().unwrap().clone()
    }
}

fn run_setup(
    username: &str,
    password: &str,
    progress: &dyn Fn(u64, u64),
    installing: &dyn Fn(),
) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("Automatic setup is only available on Windows. Build Kyber from source for this system.".to_owned());
    }
    if !is_safe_credential(username) || !is_safe_credential(password) {
        return Err("The hosting name or password contains unsupported characters.".to_owned());
    }
    let cache = Config::dir().join("cache");
    let installer = cache.join(INSTALLER_NAME);
    download_verified(INSTALLER_URL, &installer, INSTALLER_SHA256, progress)?;

    installing();
    let task_path = cache.join("install-task.toml");
    let result_path = cache.join("install-result.txt");
    let _ = std::fs::remove_file(&result_path);
    let task = InstallTask {
        installer: installer.clone(),
        username: username.to_owned(),
        password: password.to_owned(),
        result: result_path.clone(),
    };
    std::fs::write(
        &task_path,
        toml::to_string(&task).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    let launcher = std::env::current_exe().map_err(|e| e.to_string())?;
    let elevated = crate::winproc::run_elevated(
        &launcher,
        &format!("--admin-install \"{}\"", task_path.display()),
    );
    // Normally the elevated side removes the task; make sure the password
    // never stays on disk if it did not get that far.
    let _ = std::fs::remove_file(&task_path);
    elevated?;

    let result = std::fs::read_to_string(&result_path).unwrap_or_default();
    let _ = std::fs::remove_file(&result_path);
    match result.trim() {
        "ok" => {
            // 63 MB that is no longer needed.
            let _ = std::fs::remove_file(&installer);
            Ok(())
        }
        "" => Err("The setup ended without reporting a result.".to_owned()),
        error => Err(error.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("godrays-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let dir = temp_dir("sha");
        let file = dir.join("abc.txt");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(sha256_file(&file).unwrap(), sha256_hex(b"abc"));
    }

    #[test]
    fn generated_passwords_are_safe_and_differ() {
        let first = generate_password();
        let second = generate_password();
        assert_eq!(first.len(), 14);
        assert!(is_safe_credential(&first), "{first}");
        assert_ne!(first, second);
    }

    #[test]
    fn only_plain_credentials_reach_the_installer() {
        assert!(is_safe_credential("kyber"));
        assert!(is_safe_credential("abcd-2345-wxyz"));
        for bad in [
            "",
            "two words",
            "quo\"te",
            "dollar$",
            "back`tick",
            "semi;colon",
            "é",
        ] {
            assert!(!is_safe_credential(bad), "{bad}");
        }
        assert!(!is_safe_credential(&"a".repeat(65)));
    }

    /// Serves `body` once over plain HTTP on a local port.
    fn serve_once(body: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.bin", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        });
        url
    }

    #[test]
    fn downloads_are_kept_only_when_the_checksum_matches() {
        let dir = temp_dir("download");
        let body: &'static [u8] = b"pretend this is an installer";

        let dest = dir.join("good.exe");
        let seen = Mutex::new((0u64, 0u64));
        download_verified(
            &serve_once(body),
            &dest,
            &sha256_hex(body),
            &|received, total| *seen.lock().unwrap() = (received, total),
        )
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(
            *seen.lock().unwrap(),
            (body.len() as u64, body.len() as u64)
        );

        // Already present with the right hash: no request is made at all
        // (the URL points at a port nobody listens on).
        download_verified(
            "http://127.0.0.1:9/none",
            &dest,
            &sha256_hex(body),
            &|_, _| {},
        )
        .unwrap();

        let bad = dir.join("bad.exe");
        let error = download_verified(&serve_once(body), &bad, &sha256_hex(b"other"), &|_, _| {})
            .unwrap_err();
        assert!(error.contains("checksum"), "{error}");
        assert!(!bad.exists());
        assert!(!bad.with_extension("part").exists());
    }

    /// Uses the stand-in client as a stand-in installer; see `kyclient.rs`
    /// for how FAKE_KYCLIENT is provided.
    #[test]
    fn the_installer_is_run_silently_with_the_given_sign_in() {
        let Some(fake) = std::env::var_os("FAKE_KYCLIENT").map(PathBuf::from) else {
            return;
        };
        let dir = temp_dir("install");
        let args_file = dir.join("args.txt");
        let hash = sha256_file(&fake).unwrap();
        let env = [
            ("FAKE_KYCLIENT_OUT", args_file.to_str().unwrap()),
            ("FAKE_KYCLIENT_SLEEP_MS", "0"),
        ];
        let task = |username: &str, password: &str| InstallTask {
            installer: fake.clone(),
            username: username.into(),
            password: password.into(),
            result: dir.join("result.txt"),
        };

        run_installer(&task("kyber", "abcd-2345-wxyz"), &hash, &env).unwrap();
        assert_eq!(
            std::fs::read_to_string(&args_file)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "/S",
                "/BASIC=1",
                "/BASICUSER=kyber",
                "/BASICPASS=abcd-2345-wxyz",
                "/STARTMODE=none"
            ]
        );

        // A swapped installer or an unsafe value is refused before anything runs.
        std::fs::remove_file(&args_file).unwrap();
        assert!(run_installer(&task("kyber", "abcd-2345-wxyz"), &sha256_hex(b"x"), &env).is_err());
        assert!(run_installer(&task("kyber", "pass word"), &hash, &env).is_err());
        assert!(run_installer(&task("ky\"ber", "abcd"), &hash, &env).is_err());
        assert!(!args_file.exists());

        // A failing installer is reported.
        let failing = [("FAKE_KYCLIENT_SLEEP_MS", "0"), ("FAKE_KYCLIENT_EXIT", "5")];
        let error = run_installer(&task("kyber", "abcd"), &hash, &failing).unwrap_err();
        assert!(error.contains("code 5"), "{error}");
    }

    /// Network check, run on request: `cargo test -- --ignored https`.
    /// Fetches a small text file over HTTPS; the checksum is wrong on
    /// purpose, so reaching the checksum error proves the transfer worked.
    #[test]
    #[ignore]
    fn https_downloads_reach_the_checksum_step() {
        let dir = temp_dir("https");
        let dest = dir.join("readme.md");
        let received = Mutex::new(0u64);
        let error = download_verified(
            "https://gitlab.com/kyber/kyber/-/raw/main/README.md",
            &dest,
            &sha256_hex(b"not this"),
            &|bytes, _| *received.lock().unwrap() = bytes,
        )
        .unwrap_err();
        assert!(error.contains("checksum"), "{error}");
        assert!(*received.lock().unwrap() > 1000);
        assert!(!dest.exists());
    }

    #[test]
    fn the_admin_entry_point_always_deletes_the_task_file() {
        let dir = temp_dir("task");
        let task_path = dir.join("task.toml");
        let result_path = dir.join("result.txt");
        let task = InstallTask {
            installer: dir.join("missing.exe"),
            username: "kyber".into(),
            password: "abcd-2345-wxyz".into(),
            result: result_path.clone(),
        };
        std::fs::write(&task_path, toml::to_string(&task).unwrap()).unwrap();
        assert_eq!(admin_install(&task_path), 1);
        assert!(!task_path.exists());
        assert!(std::fs::read_to_string(&result_path)
            .unwrap()
            .contains("could not be read"));

        std::fs::write(&task_path, "not a task").unwrap();
        assert_eq!(admin_install(&task_path), 2);
        assert!(!task_path.exists());
    }
}
