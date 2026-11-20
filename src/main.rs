//! Godrays: a couch-friendly game-streaming launcher built on the Kyber
//! streaming client. It keeps a list of hosts and stream settings, and starts
//! `kyclient` with the matching command line.

// No console window behind the launcher in release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod agent;
mod app;
mod beacon;
mod config;
mod discovery;
mod display;
mod fx;
mod host;
mod hotkey;
mod kyber;
mod kyclient;
mod lab;
mod logtail;
mod metrics;
mod net;
mod pad;
mod secrets;
mod ui;
mod winproc;
mod winutil;

use std::path::PathBuf;

use eframe::egui;

const USAGE: &str = "\
Godrays

Usage: godrays [OPTIONS]

Options:
  --connect <HOST>      Connect to a saved host (by name or address) at startup
  --host                Start hosting this PC at startup
  --setup               Install Kyber right away if it is missing
  --lab <WHICH>         Run a latency test at startup: measure, fastest, other
  --exit-after-lab      Print the test results and close when it is done
  --fullscreen          Start Godrays fullscreen
  --demo                Show sample hosts; never reads or writes saved settings
  --screenshot <FILE>   Save a PNG of the window and exit (for UI checks)
  --screen <NAME>       Screen to open with --screenshot:
                        home, hosts, host, setup, settings, network,
                        launcher, latency, pairing, paircode, editor,
                        input, confirm, ended
  --screenshot-after <MS>  Delay before the screenshot (default 400)
  --find-encoder        Measure every encoder and codec pair on this PC and exit
  -h, --help            Show this help
";

fn main() -> eframe::Result {
    let mut args = app::StartupArgs {
        connect: None,
        screenshot: None,
        screen: None,
        screenshot_after_ms: 400,
        demo: false,
        setup: false,
        host: false,
        lab: None,
        exit_after_lab: false,
    };
    let mut fullscreen = false;
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--connect" => args.connect = argv.next(),
            "--screenshot" => args.screenshot = argv.next().map(PathBuf::from),
            "--screen" => args.screen = argv.next(),
            "--screenshot-after" => {
                args.screenshot_after_ms = argv.next().and_then(|v| v.parse().ok()).unwrap_or(400)
            }
            "--demo" => args.demo = true,
            "--setup" => args.setup = true,
            "--host" => args.host = true,
            "--lab" => args.lab = argv.next(),
            "--exit-after-lab" => args.exit_after_lab = true,
            // Internal: the elevated half of the Kyber setup. Never shows a window.
            "--admin-install" => {
                let code = match argv.next() {
                    Some(task) => kyber::admin_install(std::path::Path::new(&task)),
                    None => 2,
                };
                std::process::exit(code);
            }
            "--fullscreen" => fullscreen = true,
            // Diagnostic: run the encoder self-test and print the outcome.
            "--find-encoder" => std::process::exit(find_encoder()),
            // Developer tool: regenerate assets/godrays.ico (and .png).
            "--export-icon" => {
                let path = argv.next().unwrap_or_else(|| "assets/godrays.ico".into());
                match ui::export_icon(std::path::Path::new(&path)) {
                    Ok(()) => std::process::exit(0),
                    Err(error) => {
                        eprintln!("Could not write the icon: {error}");
                        std::process::exit(1);
                    }
                }
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            other => {
                eprintln!("Unknown argument: {other}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    // Only the fullscreen preference is needed before the window exists; the
    // app loads (and reports problems with) the full config itself.
    if !args.demo {
        fullscreen |= config::Config::load()
            .map(|c| c.launcher_fullscreen)
            .unwrap_or(false);
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Godrays")
            .with_inner_size([1240.0, 780.0])
            .with_icon(ui::window_icon())
            .with_min_inner_size([760.0, 560.0])
            .with_fullscreen(fullscreen),
        ..Default::default()
    };
    eframe::run_native(
        "Godrays",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, args)))),
    )
}

/// Runs the fastest-encoder search from a terminal and prints the results.
fn find_encoder() -> i32 {
    let client = kyclient::locate("");
    let install = kyber::install_dir(client.as_deref());
    if client.is_none() || install.is_none() {
        println!("Kyber is not installed.");
        return 2;
    }
    let config = config::Config::load().unwrap_or_default();
    let lab = lab::Lab::start(
        lab::Plan::FindFastest,
        install,
        client,
        config.stream,
        None,
        config.hosting,
        || {},
    );
    let mut last = String::new();
    loop {
        match lab.stage() {
            lab::Stage::Running { what, step, steps } => {
                let line = format!("[{step}/{steps}] {what}");
                if line != last {
                    println!("{line}");
                    last = line;
                }
            }
            lab::Stage::Done(results) => {
                for result in &results {
                    println!("{:<28} {}", result.label, result.headline());
                    if let Some(p) = &result.pipeline {
                        println!(
                            "    encode {:.1}  network {:.1}  decode {:.1}  render {:.1}  ({:.0} fps, {} frames)",
                            p.encode, p.network, p.decode, p.render, p.fps, p.frames
                        );
                    }
                }
                return if results.iter().any(|r| r.played) {
                    0
                } else {
                    1
                };
            }
            lab::Stage::Failed(error) => {
                println!("{error}");
                return 1;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
