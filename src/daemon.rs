//! Daemon lifecycle and command-facing status helpers.

use anyhow::{bail, Result};

use crate::app::{self, AppConfig};
use crate::{ipc, tmux};

pub fn start(mut config: AppConfig) -> Result<()> {
    if tmux::origin_socket_path().is_none() {
        bail!("daemon start requires an originating tmux server socket");
    }
    config.ipc.socket_path = ipc::resolve_daemon_socket_path(&config.ipc.socket_path);
    if ipc::daemon_is_healthy(&config.ipc.socket_path) {
        return Ok(());
    }
    if ipc::daemon_socket_is_live(&config.ipc.socket_path) {
        bail!("a live but incompatible service already owns the Ilmari daemon socket");
    }
    if !cfg!(feature = "socket") {
        bail!("daemon requires socket support; rebuild with feature `socket`");
    }
    config.ipc.enabled = true;
    app::run_daemon(config)
}

pub fn stop(mut config: AppConfig) -> Result<()> {
    config.ipc.socket_path = ipc::resolve_daemon_socket_path(&config.ipc.socket_path);
    if ipc::daemon_is_healthy(&config.ipc.socket_path) {
        let _ = ipc::request_daemon_stop(&config.ipc.socket_path);
    }
    if tmux::server_is_alive() {
        tmux::clear_published_state();
    }
    Ok(())
}

pub fn daemon_status(mut config: AppConfig) -> Result<()> {
    config.ipc.socket_path = ipc::resolve_daemon_socket_path(&config.ipc.socket_path);
    if ipc::daemon_is_healthy(&config.ipc.socket_path) {
        println!("running");
    } else {
        println!("stopped");
    }
    Ok(())
}

pub fn compact_status(mut config: AppConfig) -> Result<()> {
    config.ipc.socket_path = ipc::resolve_daemon_socket_path(&config.ipc.socket_path);
    if !ipc::daemon_is_healthy(&config.ipc.socket_path) {
        return Ok(());
    }
    if tmux::global_option("@ilmari_status_enabled").ok().is_some_and(|value| {
        matches!(value.trim().to_ascii_lowercase().as_str(), "0" | "off" | "false" | "no")
    }) {
        return Ok(());
    }
    if let Ok(summary) = tmux::global_option("@ilmari_status_summary") {
        if !summary.is_empty() {
            println!("{summary}");
        }
    }
    Ok(())
}
