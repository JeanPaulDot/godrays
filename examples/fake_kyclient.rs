//! Stand-in for kyclient, used to test the launcher without a Kyber install.
//!
//! Records its arguments, prints a few log-like lines, waits and exits:
//!   FAKE_KYCLIENT_OUT       file that receives one argument per line
//!   FAKE_KYCLIENT_SLEEP_MS  how long to "stream" (default 1000)
//!   FAKE_KYCLIENT_EXIT      exit code (default 0)
//!
//! It can also pose as the server (kycontroller):
//!   FAKE_SERVER_LISTEN           TCP port to accept connections on
//!   FAKE_SERVER_HELPER_PID_FILE  start a helper process and write its pid here
//!   FAKE_SERVER_GRACEFUL_FILE    exit 0 when KYBER_SHUTDOWN_EVENT is signalled,
//!                                after writing a note to this file
//!
//! Like the real client it writes to KYBER_LOG_DIR/kyclient.log:
//!   FAKE_KYCLIENT_PLAYS         log that video started
//!   FAKE_KYCLIENT_RECONNECTS    log that the connection was lost
//!   FAKE_KYCLIENT_UNAUTHORIZED  log a refused sign-in and exit 1

use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("Streaming client 0.0.0-fake");
        return;
    }
    if let Ok(path) = std::env::var("FAKE_KYCLIENT_OUT") {
        std::fs::write(path, args.join("\n")).expect("write args file");
    }
    println!("INFO  fake: args {}", args.join(" "));
    let _listener = std::env::var("FAKE_SERVER_LISTEN")
        .ok()
        .and_then(|port| std::net::TcpListener::bind(format!("127.0.0.1:{port}")).ok());
    if let Ok(pid_file) = std::env::var("FAKE_SERVER_HELPER_PID_FILE") {
        let helper = std::process::Command::new(std::env::current_exe().unwrap())
            .env_remove("FAKE_SERVER_LISTEN")
            .env_remove("FAKE_SERVER_HELPER_PID_FILE")
            .env_remove("FAKE_KYCLIENT_OUT")
            .env("FAKE_KYCLIENT_SLEEP_MS", "60000")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start helper");
        std::fs::write(pid_file, helper.id().to_string()).expect("write pid file");
    }
    if let Ok(marker) = std::env::var("FAKE_SERVER_GRACEFUL_FILE") {
        wait_for_shutdown_event(marker);
    }
    if std::env::var("FAKE_KYCLIENT_UNAUTHORIZED").is_ok() {
        log_line("ERROR kyclient::host_service - POST failed: HTTP status client error (401 Unauthorized)");
        std::process::exit(1);
    }
    if std::env::var("FAKE_KYCLIENT_PLAYS").is_ok() {
        std::thread::sleep(Duration::from_millis(300));
        log_line("INFO kyclient - Player started");
        if args.iter().any(|a| a == "--metrics") {
            write_metrics();
        }
    }
    if std::env::var("FAKE_KYCLIENT_RECONNECTS").is_ok() {
        std::thread::sleep(Duration::from_millis(300));
        log_line("INFO kyclient::event_loop - Reconnecting... (attempt 1/5)");
    }
    let sleep_ms = env_number("FAKE_KYCLIENT_SLEEP_MS", 1000);
    let exit = env_number("FAKE_KYCLIENT_EXIT", 0) as i32;
    println!(
        "INFO  fake: connecting to {}",
        args.last().map(String::as_str).unwrap_or("?")
    );
    eprintln!("\u{1b}[33mWARN\u{1b}[0m  fake: this is not a real stream");
    std::thread::sleep(Duration::from_millis(sleep_ms));
    if exit != 0 {
        eprintln!("ERROR fake: Connect failed: Unauthorized");
    }
    std::process::exit(exit);
}

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Appends a line to the log file the real client would write.
fn log_line(text: &str) {
    use std::io::Write;
    let Ok(dir) = std::env::var("KYBER_LOG_DIR") else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = std::path::Path::new(&dir).join("kyclient.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "2026-01-01T00:00:00.000 {text}");
    }
}

/// Exits cleanly when the launcher signals the shutdown event, as the real
/// server does.
#[cfg(windows)]
fn wait_for_shutdown_event(marker: String) {
    let Some(handle) = std::env::var("KYBER_SHUTDOWN_EVENT")
        .ok()
        .and_then(|value| value.parse::<isize>().ok())
    else {
        return;
    };
    std::thread::spawn(move || {
        // SAFETY: the handle was inherited from the launcher, which keeps
        // it open for as long as this process runs.
        let result = unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(
                handle as *mut std::ffi::c_void,
                u32::MAX,
            )
        };
        if result == 0 {
            std::fs::write(marker, "shutdown event received").expect("write marker");
            std::process::exit(0);
        }
    });
}

#[cfg(not(windows))]
fn wait_for_shutdown_event(_marker: String) {}

/// Like the real client with --metrics: per-frame stage timestamps as JSON
/// lines in metrics.json, in the working directory. 120 frames, 6 ms apart,
/// 8 ms from capture to display.
fn write_metrics() {
    use std::io::Write;
    let Ok(mut file) = std::fs::File::create("metrics.json") else {
        return;
    };
    let _ = writeln!(
        file,
        r#"{{"log_type":"raw_metric","type":"network_ping","offset_micros":0,"delay_micros":100}}"#
    );
    for frame in 0..120i64 {
        let base = 1_000_000 + frame * 6_000;
        for (event, at) in [
            ("acquired", 0),
            ("encoded", 4_000),
            ("sent", 4_100),
            ("received", 4_600),
            ("decoded", 4_900),
            ("displayed", 8_000),
        ] {
            let _ = writeln!(
                file,
                r#"{{"log_type":"raw_metric","type":"video","event":"{event}","source_id":1,"pts":{frame},"ts":{}}}"#,
                base + at
            );
        }
    }
}
