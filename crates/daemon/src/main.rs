//! Headless kymido daemon binary entrypoint, following the daemon config
//! pattern from server.rs.
//!
//! Signal handling:
//! - SIGINT (Ctrl-C) and SIGTERM trigger graceful shutdown via Daemon::shutdown()
//! - The Drop implementation handles cleanup if signals are not caught
//!
//! No systemd/Windows service integration on purpose: the daemon stays
//! portable and embeddable.

use config::Config;
use daemon::{Daemon, DaemonConfig, DaemonError};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Main entry point for the kymido daemon binary: build a `DaemonConfig`
/// from `Config::daemon_socket_path` / `Config::session_db_path`, start
/// the daemon, block until shutdown is triggered, then clean up.
fn main() -> Result<(), DaemonError> {
    let config = Config::load().map_err(DaemonError::Config)?;

    let cfg = DaemonConfig::from_config(&config)?;

    println!("Starting kymido daemon...");
    let mut daemon = Daemon::start(cfg)?;

    println!(
        "Daemon started on socket: {:?}",
        daemon.socket_addr().path()
    );
    println!("Daemon PID: {}", daemon.pid());

    spawn_startup_sweep(&config);

    // Shared flag to track if shutdown was requested via signal
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let shutdown_flag = Arc::clone(&shutdown_requested);

    // Set up signal handlers for SIGINT and SIGTERM
    ctrlc::set_handler(move || {
        shutdown_flag.store(true, Ordering::SeqCst);
    })
    .expect("Failed to set signal handler");

    println!("Daemon running. Press Ctrl-C to stop.");

    // Stop on either a process signal or a daemon.shutdown request.
    while !shutdown_requested.load(Ordering::SeqCst) && !daemon.is_shutdown_requested() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    println!("Shutdown requested, stopping daemon...");
    daemon.shutdown();
    drop(daemon);
    println!("Daemon stopped cleanly.");

    Ok(())
}

/// Spawn the one-shot idle-session sweep as a detached child process.
///
/// Runs once per daemon start, never on a timer: startup is the natural
/// checkpoint, and a background reaper that can wake up under an active
/// session is exactly the surprise we do not want. What to run (or whether
/// to run at all) is decided by [`daemon::startup_sweep::startup_sweep_command`];
/// this function only keeps the spawn and the child-reaping glue.
fn spawn_startup_sweep(config: &Config) {
    let Ok(db_path) = config.session_db_path() else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    let cmd = match daemon::startup_sweep::startup_sweep_command(&db_path, dir) {
        Ok(cmd) => cmd,
        // Fresh install: nothing has been stored yet. Silent, not an error.
        Err(daemon::startup_sweep::SweepSkip::NoDatabase) => return,
        Err(daemon::startup_sweep::SweepSkip::CliMissing(cli)) => {
            println!(
                "idle-session sweep skipped: kymido CLI not found at {}",
                cli.display()
            );
            return;
        }
    };
    let spawn = std::process::Command::new(&cmd.program)
        .args(cmd.args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawn {
        // The wait thread exists to reap the child: a daemon that never
        // wait()s leaves one zombie behind per start, forever.
        Ok(mut child) => {
            let pid = child.id();
            std::thread::spawn(move || match child.wait() {
                Ok(status) if status.success() => {}
                Ok(status) => println!("idle-session sweep exited with {status}"),
                Err(e) => println!("idle-session sweep wait failed: {e}"),
            });
            println!("idle-session sweep started (pid {pid})");
        }
        Err(e) => println!("idle-session sweep could not start: {e}"),
    }
}
