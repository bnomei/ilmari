#!/usr/bin/env bash
# Isolated real-daemon lifecycle and exact tmux-socket routing coverage.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ilmari_bin="${ILMARI_BIN:-$repo_root/target/debug/ilmari}"
# Keep the nested Ilmari socket below macOS's short Unix-domain path limit.
tmp_dir="$(mktemp -d "/tmp/ilmari,daemon.XXXXXX")"
tmux_socket="$tmp_dir/tmux.sock"
runtime_dir="$tmp_dir/runtime"
config_dir="$tmp_dir/config"
state_dir="$tmp_dir/state"
daemon_pid=''
foreign_pid=''

cleanup() {
  if [[ -n "$foreign_pid" ]]; then
    kill "$foreign_pid" 2>/dev/null || true
    wait "$foreign_pid" 2>/dev/null || true
  fi
  if [[ -n "$daemon_pid" ]]; then
    kill "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  tmux -S "$tmux_socket" kill-server 2>/dev/null || true
  rm -rf "$tmp_dir"
}
trap cleanup EXIT INT TERM

fail() {
  printf 'tmux daemon integration: %s\n' "$*" >&2
  exit 1
}

[[ -x "$ilmari_bin" ]] || fail "binary not found: $ilmari_bin (run cargo build --all-features)"
mkdir -m 700 -p "$runtime_dir" "$config_dir" "$state_dir"
tmux -S "$tmux_socket" new-session -d -s ilmari-daemon 'sleep 120'

refresh_tmux_context() {
  server_pid="$(tmux -S "$tmux_socket" display-message -p '#{pid}')"
  tmux_context="$tmux_socket,$server_pid,0"
}

refresh_tmux_context

run_ilmari() {
  TMUX="$tmux_context" XDG_RUNTIME_DIR="$runtime_dir" \
    XDG_CONFIG_HOME="$config_dir" XDG_STATE_HOME="$state_dir" \
    "$ilmari_bin" "$@"
}

# A popup that publishes first owns the compatibility URL. Daemon startup must
# publish only its dedicated URL and leave that live popup discoverable.
popup_mcp_url='http://127.0.0.1:64020/mcp'
tmux -S "$tmux_socket" set-option -g @ilmari_mcp_url "$popup_mcp_url"
run_ilmari daemon start --mcp --mcp-port 0 >"$tmp_dir/daemon.log" 2>&1 &
daemon_pid="$!"
for _ in {1..100}; do
  [[ "$(run_ilmari daemon status)" == 'running' ]] && break
  sleep 0.05
done
if [[ "$(run_ilmari daemon status)" != 'running' ]]; then
  sed -n '1,120p' "$tmp_dir/daemon.log" >&2
  fail 'daemon did not become healthy'
fi

# A compatible second start must succeed without replacing the collector.
run_ilmari daemon start
kill -0 "$daemon_pid" 2>/dev/null || fail 'singleton start replaced or stopped the daemon'

# A tmux restart can reuse the same socket pathname but creates a new server
# generation. The old daemon must be identified as an owned incompatible peer,
# stopped cooperatively, and replaced without needing another plugin reload.
old_daemon_pid="$daemon_pid"
tmux -S "$tmux_socket" kill-server 2>/dev/null || true
tmux -S "$tmux_socket" new-session -d -s ilmari-daemon-restarted 'sleep 120'
refresh_tmux_context
# Server-global options are intentionally lost with the old server. Restore the
# popup-owned legacy URL before the replacement daemon publishes its own state.
tmux -S "$tmux_socket" set-option -g @ilmari_mcp_url "$popup_mcp_url"
run_ilmari daemon start --mcp --mcp-port 0 >"$tmp_dir/restarted-daemon.log" 2>&1 &
daemon_pid="$!"
# Loaded CI runners can take longer than the daemon's internal stale-peer
# recovery window before the replacement publishes its first healthy snapshot.
for _ in {1..200}; do
  [[ "$(run_ilmari daemon status)" == 'running' ]] && break
  sleep 0.05
done
if [[ "$(run_ilmari daemon status)" != 'running' ]]; then
  sed -n '1,120p' "$tmp_dir/restarted-daemon.log" >&2
  fail 'daemon did not recover after tmux server replacement'
fi
for _ in {1..100}; do
  kill -0 "$old_daemon_pid" 2>/dev/null || break
  sleep 0.05
done
kill -0 "$old_daemon_pid" 2>/dev/null \
  && fail 'tmux server replacement did not stop the stale daemon'
wait "$old_daemon_pid" 2>/dev/null || true

# Learn the default daemon target, then deliberately move the owned collector
# elsewhere. A regular Ilmari IPC server now occupies the old target as a
# foreign peer. Starting the default daemon again must reject that requested
# target before it stops the healthy owned collector at the replacement path.
foreign_daemon_socket="$(tmux -S "$tmux_socket" show-option -gqv @ilmari_daemon_socket_path)"
[[ "$foreign_daemon_socket" == "$runtime_dir"/* ]] || fail 'default daemon socket was not published'
old_configured_socket="$runtime_dir/old-config.sock"
old_daemon_pid="$daemon_pid"
run_ilmari daemon start --socket-path "$old_configured_socket" --mcp --mcp-port 0 \
  >"$tmp_dir/old-daemon.log" 2>&1 &
daemon_pid="$!"
for _ in {1..100}; do
  replacement_socket="$(tmux -S "$tmux_socket" show-option -gqv @ilmari_daemon_socket_path)"
  [[ "$replacement_socket" != "$foreign_daemon_socket" ]] \
    && [[ "$(run_ilmari daemon status)" == 'running' ]] && break
  sleep 0.05
done
[[ "$replacement_socket" != "$foreign_daemon_socket" ]] \
  && [[ "$(run_ilmari daemon status)" == 'running' ]] \
  || fail 'replacement daemon did not become healthy'
kill -0 "$old_daemon_pid" 2>/dev/null && fail 'path replacement did not stop the original daemon'

TMUX="$tmux_context" XDG_RUNTIME_DIR="$runtime_dir" \
  XDG_CONFIG_HOME="$config_dir" XDG_STATE_HOME="$state_dir" \
  "$ilmari_bin" --no-tui --no-git --socket-path "$foreign_daemon_socket" \
  >"$tmp_dir/foreign.log" 2>&1 &
foreign_pid="$!"
for _ in {1..100}; do
  [[ -S "$foreign_daemon_socket" ]] && break
  sleep 0.05
done
[[ -S "$foreign_daemon_socket" ]] || fail 'foreign requested daemon socket did not become live'
if run_ilmari daemon start --mcp --mcp-port 0 >"$tmp_dir/foreign-rejection.log" 2>&1; then
  fail 'daemon start unexpectedly replaced the owned daemon despite a foreign requested socket'
fi
kill -0 "$daemon_pid" 2>/dev/null \
  || fail 'foreign requested socket stopped the healthy owned daemon'
[[ "$(run_ilmari daemon status)" == 'running' ]] \
  || fail 'foreign requested socket left the healthy owned daemon unavailable'

kill "$foreign_pid" 2>/dev/null || true
wait "$foreign_pid" 2>/dev/null || true
foreign_pid=''
# A SIGTERMed foreign process may leave its Unix pathname behind, but it no
# longer has a listener. The daemon bind path must classify that as unreachable
# and safely reclaim the stale entry.
run_ilmari daemon start --mcp --mcp-port 0 >"$tmp_dir/daemon.log" 2>&1 &
daemon_pid="$!"
for _ in {1..100}; do
  recovered_socket="$(tmux -S "$tmux_socket" show-option -gqv @ilmari_daemon_socket_path)"
  [[ "$recovered_socket" == "$foreign_daemon_socket" ]] \
    && [[ "$(run_ilmari daemon status)" == 'running' ]] && break
  sleep 0.05
done
[[ "$recovered_socket" == "$foreign_daemon_socket" ]] \
  && [[ "$(run_ilmari daemon status)" == 'running' ]] \
  || {
    sed -n '1,120p' "$tmp_dir/daemon.log" >&2
    fail 'default daemon did not recover after foreign socket release'
  }

published_socket="$(tmux -S "$tmux_socket" show-option -gqv @ilmari_socket_path)"
[[ "$published_socket" == "$runtime_dir"/* ]] || fail 'socket was not scoped to test runtime'
daemon_mcp_url="$(tmux -S "$tmux_socket" show-option -gqv @ilmari_daemon_mcp_url)"
[[ "$daemon_mcp_url" == http://127.0.0.1:*/mcp ]] \
  || fail 'daemon-specific MCP URL was not published'
[[ "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_mcp_url)" == "$popup_mcp_url" ]] \
  || fail 'daemon startup overwrote popup-first legacy MCP URL'
[[ -z "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_daemon_legacy_mcp_url)" ]] \
  || fail 'daemon claimed popup-owned legacy MCP URL'

# Seed every global render counter the daemon may publish so stop cleanup can
# prove full parity with Rust clear_published_state (and TPM fallback).
for option in \
  @ilmari_window_badges \
  @ilmari_status_summary \
  @ilmari_running_count \
  @ilmari_waiting_count \
  @ilmari_finished_count \
  @ilmari_attention_count \
  @ilmari_waiting_state_count \
  @ilmari_finished_state_count \
  @ilmari_terminated_count \
  @ilmari_unknown_count; do
  tmux -S "$tmux_socket" set-option -g "$option" 'stale'
done
tmux -S "$tmux_socket" set-option -p @ilmari_state 'running'
tmux -S "$tmux_socket" set-option -p @ilmari_badge 'R'
tmux -S "$tmux_socket" set-option -p @ilmari_attention '1'

run_ilmari daemon stop
for _ in {1..100}; do
  kill -0 "$daemon_pid" 2>/dev/null || break
  sleep 0.05
done
kill -0 "$daemon_pid" 2>/dev/null && fail 'daemon did not stop'
daemon_pid=''
[[ "$(run_ilmari daemon status)" == 'stopped' ]] || fail 'daemon status did not become stopped'
for option in \
  @ilmari_window_badges \
  @ilmari_status_summary \
  @ilmari_running_count \
  @ilmari_waiting_count \
  @ilmari_finished_count \
  @ilmari_attention_count \
  @ilmari_waiting_state_count \
  @ilmari_finished_state_count \
  @ilmari_terminated_count \
  @ilmari_unknown_count \
  @ilmari_daemon_socket_path \
  @ilmari_daemon_owner_pid \
  @ilmari_daemon_mcp_url; do
  [[ -z "$(tmux -S "$tmux_socket" show-option -gqv "$option")" ]] \
    || fail "$option was not cleaned"
done
[[ -z "$(tmux -S "$tmux_socket" show-option -pqv @ilmari_state)" ]] \
  || fail 'pane @ilmari_state was not cleaned'
[[ -z "$(tmux -S "$tmux_socket" show-option -pqv @ilmari_badge)" ]] \
  || fail 'pane @ilmari_badge was not cleaned'
[[ -z "$(tmux -S "$tmux_socket" show-option -pqv @ilmari_attention)" ]] \
  || fail 'pane @ilmari_attention was not cleaned'
[[ "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_mcp_url)" == "$popup_mcp_url" ]] \
  || fail 'daemon cleanup removed popup-owned legacy MCP URL'
[[ -z "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_daemon_mcp_url)" ]] \
  || fail 'daemon-specific MCP URL was not cleaned'

printf '%s\n' 'tmux daemon integration: ok'
