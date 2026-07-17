//! One collector daemon per tmux server: start, stop, and status surfaces.
//!
//! The daemon is observer-only acceleration for popups and status lines. `start`
//! is idempotent against a healthy compatible incumbent on the desired path,
//! cooperatively replaces an owned healthy incumbent on a different path, replaces
//! stale ownership after a bounded wait, and refuses foreign sockets. Stop prefers
//! a cooperative shutdown request, then clears only exact published owner tokens
//! once the listener is gone.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Result};

use crate::app::{self, AppConfig};
use crate::{ipc, tmux};

/// Owner pid, daemon socket path, and optional MCP URL published into tmux options.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonPublicationTuple {
    owner_pid: u32,
    socket_path: PathBuf,
    mcp_url: Option<String>,
}

/// Pure start disposition from desired path, incumbent path, and endpoint classification.
///
/// Start binds only the desired configured path. Status/stop may still resolve a
/// different published incumbent to manage it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DaemonStartDisposition {
    /// Healthy owned daemon already listening on the desired path.
    NoOpSamePath,
    /// Healthy owned daemon on a different path; stop it before binding desired.
    ReplaceOwnedHealthy { incumbent: PathBuf },
    /// Owned but unhealthy/stale/incompatible; recover then bind desired.
    RecoverOwnedUnhealthy { incumbent: PathBuf },
    /// A different requested bind path already has a listener, so preserve the incumbent.
    RefuseOccupiedDesired,
    /// Live non-Ilmari or out-of-scope endpoint; never take over.
    RefuseForeign,
    /// Nothing usable at the incumbent path; bind desired.
    BindFresh,
}

/// Start the per-tmux-server collector, or succeed if a healthy compatible daemon is already running.
///
/// # Errors
///
/// Returns an error when no originating tmux socket is known, a foreign service owns the
/// daemon path, a stale or path-mismatch daemon cannot be replaced, or the `socket` feature
/// is missing.
pub fn start(mut config: AppConfig) -> Result<()> {
    if tmux::origin_socket_path().is_none() {
        bail!("daemon start requires an originating tmux server socket");
    }
    let (desired_socket_path, incumbent_socket_path) = daemon_start_paths(&config.ipc.socket_path);
    // Check the requested bind target before considering an owned path replacement. A
    // published incumbent can be healthy while a separate process has claimed the new
    // target; stopping the incumbent first would leave the tmux server without a daemon
    // when `IpcServer::start` subsequently rejects that occupied target.
    let desired_endpoint = ipc::daemon_endpoint_state(&desired_socket_path);
    let incumbent_endpoint = if desired_socket_path == incumbent_socket_path {
        desired_endpoint
    } else {
        ipc::daemon_endpoint_state(&incumbent_socket_path)
    };
    match plan_daemon_start(
        &desired_socket_path,
        &incumbent_socket_path,
        desired_endpoint,
        incumbent_endpoint,
    ) {
        DaemonStartDisposition::NoOpSamePath => return Ok(()),
        DaemonStartDisposition::ReplaceOwnedHealthy { incumbent } => {
            stop_owned_incumbent_for_start(&incumbent, IncumbentStopKind::HealthyPathReplacement)?;
        }
        DaemonStartDisposition::RecoverOwnedUnhealthy { incumbent } => {
            // A just-bound peer may not have published revision one yet. Give start races a
            // bounded chance to converge before replacing stale/incompatible ownership.
            let mut replaced_for_path = false;
            for _ in 0..20 {
                thread::sleep(Duration::from_millis(50));
                if ipc::daemon_is_healthy(&incumbent) {
                    if desired_socket_path == incumbent {
                        return Ok(());
                    }
                    // Converged to healthy but not on the desired path — replace it.
                    stop_owned_incumbent_for_start(
                        &incumbent,
                        IncumbentStopKind::HealthyPathReplacement,
                    )?;
                    replaced_for_path = true;
                    break;
                }
            }
            if !replaced_for_path {
                stop_owned_incumbent_for_start(&incumbent, IncumbentStopKind::StaleRecovery)?;
            }
        }
        DaemonStartDisposition::RefuseForeign => {
            bail!("a live but incompatible service already owns the Ilmari daemon socket");
        }
        DaemonStartDisposition::RefuseOccupiedDesired => {
            bail!("a live service already owns the requested Ilmari daemon socket");
        }
        DaemonStartDisposition::BindFresh => {}
    }
    if !cfg!(feature = "socket") {
        bail!("daemon requires socket support; rebuild with feature `socket`");
    }
    config.ipc.socket_path = desired_socket_path;
    config.ipc.enabled = true;
    app::run_daemon(config)
}

fn plan_daemon_start(
    desired: &Path,
    incumbent: &Path,
    desired_endpoint: ipc::DaemonEndpointState,
    incumbent_endpoint: ipc::DaemonEndpointState,
) -> DaemonStartDisposition {
    if desired != incumbent && desired_endpoint != ipc::DaemonEndpointState::Unreachable {
        return DaemonStartDisposition::RefuseOccupiedDesired;
    }

    match incumbent_endpoint {
        ipc::DaemonEndpointState::Healthy if desired == incumbent => {
            DaemonStartDisposition::NoOpSamePath
        }
        ipc::DaemonEndpointState::Healthy => {
            DaemonStartDisposition::ReplaceOwnedHealthy { incumbent: incumbent.to_path_buf() }
        }
        ipc::DaemonEndpointState::OwnedUnhealthy => {
            DaemonStartDisposition::RecoverOwnedUnhealthy { incumbent: incumbent.to_path_buf() }
        }
        ipc::DaemonEndpointState::Foreign => DaemonStartDisposition::RefuseForeign,
        ipc::DaemonEndpointState::Unreachable => DaemonStartDisposition::BindFresh,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncumbentStopKind {
    /// Owned healthy daemon must move to a new desired socket path.
    HealthyPathReplacement,
    /// Stale/incompatible owned daemon recovered before bind.
    StaleRecovery,
}

/// Cooperatively stop an owned incumbent, wait for listener removal, and optionally
/// clear ownership-safe published tokens (healthy path replacement only).
fn stop_owned_incumbent_for_start(incumbent: &Path, kind: IncumbentStopKind) -> Result<()> {
    let published = published_daemon_tuple();
    let live_owner_pid = ipc::daemon_owner_pid(incumbent);
    let cleanup = cleanup_tuple_for_stop(live_owner_pid, incumbent, published);

    if !ipc::request_daemon_stop(incumbent) {
        match kind {
            IncumbentStopKind::HealthyPathReplacement => {
                bail!("failed to stop the owned Ilmari daemon for socket path replacement");
            }
            IncumbentStopKind::StaleRecovery => {
                bail!("failed to stop the stale or incompatible Ilmari daemon");
            }
        }
    }
    for _ in 0..40 {
        if !ipc::daemon_socket_is_live(incumbent) {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if ipc::daemon_socket_is_live(incumbent) {
        match kind {
            IncumbentStopKind::HealthyPathReplacement => {
                bail!("owned Ilmari daemon did not release its socket for path replacement");
            }
            IncumbentStopKind::StaleRecovery => {
                bail!("stale Ilmari daemon did not release its socket");
            }
        }
    }

    // Path replacement must clear the old publication so the new daemon can claim the
    // desired path. Stale recovery preserves the previous cleanup timing (daemon-owned
    // primary cleanup; no forced publication wipe here).
    if kind == IncumbentStopKind::HealthyPathReplacement {
        if let Some(cleanup) = cleanup.filter(|cleanup| {
            tmux::server_is_alive()
                && stop_fallback_cleanup_allowed(
                    true,
                    ipc::daemon_socket_is_live(&cleanup.socket_path),
                )
        }) {
            tmux::clear_published_state(
                cleanup.owner_pid,
                Some(&cleanup.socket_path),
                cleanup.mcp_url.as_deref(),
            );
        }
    }
    Ok(())
}

/// Desired bind path and the incumbent path that may currently own the socket.
fn daemon_start_paths(configured: &Path) -> (PathBuf, PathBuf) {
    daemon_start_paths_with(configured, ipc::resolve_daemon_socket_path)
}

fn daemon_start_paths_with(
    configured: &Path,
    resolve_incumbent: impl FnOnce(&Path) -> PathBuf,
) -> (PathBuf, PathBuf) {
    let desired = ipc::daemon_source_socket_path(configured);
    let incumbent = resolve_incumbent(&desired);
    (desired, incumbent)
}

/// Request cooperative stop of the owned daemon and clear matching published tmux tokens.
///
/// Primary cleanup belongs to the daemon process. Fallback option clearing runs only after
/// the socket listener is gone, so MCP teardown can still observe its owner token.
pub fn stop(mut config: AppConfig) -> Result<()> {
    let published = published_daemon_tuple();
    let fallback_path = ipc::daemon_source_socket_path(&config.ipc.socket_path);
    config.ipc.socket_path = ipc::resolve_daemon_socket_path(&fallback_path);
    let live_owner_pid = ipc::daemon_owner_pid(&config.ipc.socket_path);
    let cleanup = cleanup_tuple_for_stop(live_owner_pid, &config.ipc.socket_path, published);
    let stop_requested =
        live_owner_pid.is_some_and(|_| ipc::request_daemon_stop(&config.ipc.socket_path));
    if stop_requested {
        // The daemon owns the primary cleanup path. Wait until its listener is
        // gone before applying exact-value fallback cleanup, so the owner token
        // remains available while daemon-side MCP cleanup runs.
        for _ in 0..40 {
            if !ipc::daemon_socket_is_live(&config.ipc.socket_path) {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    if let Some(cleanup) = cleanup.filter(|cleanup| {
        tmux::server_is_alive()
            && stop_fallback_cleanup_allowed(
                stop_requested,
                ipc::daemon_socket_is_live(&cleanup.socket_path),
            )
    }) {
        tmux::clear_published_state(
            cleanup.owner_pid,
            Some(&cleanup.socket_path),
            cleanup.mcp_url.as_deref(),
        );
    }
    Ok(())
}

fn published_daemon_tuple() -> Option<DaemonPublicationTuple> {
    daemon_publication_tuple_from_values(
        tmux::global_option("@ilmari_daemon_owner_pid").ok().as_deref(),
        tmux::global_option("@ilmari_daemon_socket_path").ok().as_deref(),
        tmux::global_option("@ilmari_daemon_mcp_url").ok().as_deref(),
    )
}

fn daemon_publication_tuple_from_values(
    owner_pid: Option<&str>,
    socket_path: Option<&str>,
    mcp_url: Option<&str>,
) -> Option<DaemonPublicationTuple> {
    let owner_pid = owner_pid.filter(|value| !value.is_empty())?.parse().ok()?;
    let socket_path = socket_path.filter(|value| !value.is_empty())?;
    Some(DaemonPublicationTuple {
        owner_pid,
        socket_path: PathBuf::from(socket_path),
        mcp_url: mcp_url.filter(|value| !value.is_empty()).map(ToOwned::to_owned),
    })
}

/// Choose exact owner/socket/MCP values safe to clear after a stop request.
///
/// Prefers the live endpoint identity when it matches published tmux tokens so a
/// foreign daemon is never cleared by accident.
fn cleanup_tuple_for_stop(
    live_owner_pid: Option<u32>,
    live_socket_path: &Path,
    published: Option<DaemonPublicationTuple>,
) -> Option<DaemonPublicationTuple> {
    let Some(live_owner_pid) = live_owner_pid else {
        return published;
    };
    if let Some(published) = published.filter(|published| {
        published.owner_pid == live_owner_pid && published.socket_path == live_socket_path
    }) {
        return Some(published);
    }
    Some(DaemonPublicationTuple {
        owner_pid: live_owner_pid,
        socket_path: live_socket_path.to_path_buf(),
        mcp_url: None,
    })
}

fn stop_fallback_cleanup_allowed(_stop_requested: bool, socket_is_live: bool) -> bool {
    !socket_is_live
}

/// Print `running` or `stopped` for the resolved daemon endpoint (machine-friendly).
pub fn daemon_status(mut config: AppConfig) -> Result<()> {
    config.ipc.socket_path =
        ipc::resolve_daemon_socket_path(&ipc::daemon_source_socket_path(&config.ipc.socket_path));
    if ipc::daemon_is_healthy(&config.ipc.socket_path) {
        println!("running");
    } else {
        println!("stopped");
    }
    Ok(())
}

/// Print the daemon's compact status-line fragment when a healthy daemon has published one.
///
/// Silent when the daemon is down or status publication is disabled in tmux options.
pub fn compact_status(mut config: AppConfig) -> Result<()> {
    config.ipc.socket_path =
        ipc::resolve_daemon_socket_path(&ipc::daemon_source_socket_path(&config.ipc.socket_path));
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

#[cfg(test)]
mod tests {
    use super::{
        cleanup_tuple_for_stop, daemon_publication_tuple_from_values, daemon_start_paths_with,
        plan_daemon_start, stop_fallback_cleanup_allowed, DaemonPublicationTuple,
        DaemonStartDisposition,
    };
    use crate::ipc::DaemonEndpointState;
    use std::path::{Path, PathBuf};

    #[test]
    fn unhealthy_legacy_incumbent_does_not_replace_desired_bind_path() {
        let configured = Path::new("/tmp/ilmari/app.sock");
        let legacy = PathBuf::from("/tmp/ilmari/legacy.sock");
        let (desired, incumbent) = daemon_start_paths_with(configured, |_desired| legacy.clone());

        assert_eq!(incumbent, legacy);
        assert_ne!(desired, incumbent);
        assert!(desired.file_name().unwrap().to_string_lossy().starts_with("ilmari-daemon-"));
    }

    #[test]
    fn desired_endpoint_remains_both_incumbent_and_bind_target_without_legacy_daemon() {
        let configured = Path::new("/tmp/ilmari/app.sock");
        let (desired, incumbent) =
            daemon_start_paths_with(configured, |desired| desired.to_path_buf());

        assert_eq!(desired, incumbent);
    }

    #[test]
    fn same_path_healthy_start_is_noop() {
        let path = Path::new("/tmp/ilmari/ilmari-daemon-same.sock");
        assert_eq!(
            plan_daemon_start(
                path,
                path,
                DaemonEndpointState::Healthy,
                DaemonEndpointState::Healthy,
            ),
            DaemonStartDisposition::NoOpSamePath
        );
    }

    #[test]
    fn different_path_owned_healthy_incumbent_is_replaced() {
        let desired = Path::new("/tmp/ilmari/ilmari-daemon-new.sock");
        let incumbent = Path::new("/tmp/ilmari/ilmari-daemon-old.sock");
        assert_eq!(
            plan_daemon_start(
                desired,
                incumbent,
                DaemonEndpointState::Unreachable,
                DaemonEndpointState::Healthy,
            ),
            DaemonStartDisposition::ReplaceOwnedHealthy { incumbent: incumbent.to_path_buf() }
        );
    }

    #[test]
    fn unhealthy_incumbent_plans_recovery_without_binding_foreign_path() {
        let desired = Path::new("/tmp/ilmari/ilmari-daemon-new.sock");
        let incumbent = Path::new("/tmp/ilmari/legacy.sock");
        assert_eq!(
            plan_daemon_start(
                desired,
                incumbent,
                DaemonEndpointState::Unreachable,
                DaemonEndpointState::OwnedUnhealthy,
            ),
            DaemonStartDisposition::RecoverOwnedUnhealthy { incumbent: incumbent.to_path_buf() }
        );
        // Recovery targets the incumbent only; desired remains the eventual bind path.
        let (planned_desired, planned_incumbent) =
            daemon_start_paths_with(Path::new("/tmp/ilmari/app.sock"), |_| incumbent.to_path_buf());
        assert_ne!(planned_desired, planned_incumbent);
        assert_eq!(planned_incumbent, incumbent);
    }

    #[test]
    fn unowned_live_endpoint_is_refused() {
        let desired = Path::new("/tmp/ilmari/ilmari-daemon-new.sock");
        let incumbent = Path::new("/tmp/ilmari/foreign.sock");
        assert_eq!(
            plan_daemon_start(
                desired,
                incumbent,
                DaemonEndpointState::Unreachable,
                DaemonEndpointState::Foreign,
            ),
            DaemonStartDisposition::RefuseForeign
        );
        // Same-path foreign is also refused — never take over an unowned live endpoint.
        assert_eq!(
            plan_daemon_start(
                desired,
                desired,
                DaemonEndpointState::Foreign,
                DaemonEndpointState::Foreign,
            ),
            DaemonStartDisposition::RefuseForeign
        );
    }

    #[test]
    fn occupied_desired_path_refuses_before_replacing_healthy_incumbent() {
        let desired = Path::new("/tmp/ilmari/ilmari-daemon-new.sock");
        let incumbent = Path::new("/tmp/ilmari/ilmari-daemon-old.sock");

        assert_eq!(
            plan_daemon_start(
                desired,
                incumbent,
                DaemonEndpointState::Foreign,
                DaemonEndpointState::Healthy,
            ),
            DaemonStartDisposition::RefuseOccupiedDesired
        );
    }

    #[test]
    fn unreachable_incumbent_binds_fresh_on_desired_path() {
        let desired = Path::new("/tmp/ilmari/ilmari-daemon-new.sock");
        let incumbent = Path::new("/tmp/ilmari/missing.sock");
        assert_eq!(
            plan_daemon_start(
                desired,
                incumbent,
                DaemonEndpointState::Unreachable,
                DaemonEndpointState::Unreachable,
            ),
            DaemonStartDisposition::BindFresh
        );
    }

    #[test]
    fn accepted_stop_preserves_owner_metadata_until_daemon_listener_is_gone() {
        assert!(!stop_fallback_cleanup_allowed(true, true));
        assert!(stop_fallback_cleanup_allowed(true, false));
        assert!(!stop_fallback_cleanup_allowed(false, true));
        assert!(stop_fallback_cleanup_allowed(false, false));
    }

    #[test]
    fn hard_killed_daemon_recovers_exact_cleanup_tuple_without_ping() {
        let published = daemon_publication_tuple_from_values(
            Some("4242"),
            Some("/tmp/ilmari/daemon.sock"),
            Some("http://127.0.0.1:4010/mcp"),
        )
        .expect("complete tmux publication should parse");

        assert_eq!(
            cleanup_tuple_for_stop(None, Path::new("/tmp/derived.sock"), Some(published)),
            Some(DaemonPublicationTuple {
                owner_pid: 4242,
                socket_path: PathBuf::from("/tmp/ilmari/daemon.sock"),
                mcp_url: Some("http://127.0.0.1:4010/mcp".to_string()),
            })
        );
    }

    #[test]
    fn incomplete_or_invalid_publication_is_not_used_for_dead_cleanup() {
        assert!(daemon_publication_tuple_from_values(
            None,
            Some("/tmp/daemon.sock"),
            Some("http://127.0.0.1:4010/mcp")
        )
        .is_none());
        assert!(daemon_publication_tuple_from_values(
            Some("not-a-pid"),
            Some("/tmp/daemon.sock"),
            None
        )
        .is_none());
    }

    #[test]
    fn path_replacement_cleanup_tuple_prefers_matching_live_and_published() {
        let published = daemon_publication_tuple_from_values(
            Some("99"),
            Some("/tmp/ilmari/old.sock"),
            Some("http://127.0.0.1:9/mcp"),
        )
        .expect("published");
        assert_eq!(
            cleanup_tuple_for_stop(Some(99), Path::new("/tmp/ilmari/old.sock"), Some(published)),
            Some(DaemonPublicationTuple {
                owner_pid: 99,
                socket_path: PathBuf::from("/tmp/ilmari/old.sock"),
                mcp_url: Some("http://127.0.0.1:9/mcp".to_string()),
            })
        );
    }

    #[test]
    fn path_replacement_cleanup_without_matching_publication_uses_live_only() {
        let published = daemon_publication_tuple_from_values(
            Some("1"),
            Some("/tmp/other.sock"),
            Some("http://127.0.0.1:1/mcp"),
        )
        .expect("published");
        assert_eq!(
            cleanup_tuple_for_stop(Some(42), Path::new("/tmp/ilmari/old.sock"), Some(published)),
            Some(DaemonPublicationTuple {
                owner_pid: 42,
                socket_path: PathBuf::from("/tmp/ilmari/old.sock"),
                mcp_url: None,
            })
        );
    }
}
