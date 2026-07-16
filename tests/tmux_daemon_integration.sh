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

cleanup() {
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
server_pid="$(tmux -S "$tmux_socket" display-message -p '#{pid}')"
tmux_context="$tmux_socket,$server_pid,0"

run_ilmari() {
  TMUX="$tmux_context" XDG_RUNTIME_DIR="$runtime_dir" \
    XDG_CONFIG_HOME="$config_dir" XDG_STATE_HOME="$state_dir" \
    "$ilmari_bin" "$@"
}

run_ilmari daemon start >"$tmp_dir/daemon.log" 2>&1 &
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

published_socket="$(tmux -S "$tmux_socket" show-option -gqv @ilmari_socket_path)"
[[ "$published_socket" == "$runtime_dir"/* ]] || fail 'socket was not scoped to test runtime'

run_ilmari daemon stop
for _ in {1..100}; do
  kill -0 "$daemon_pid" 2>/dev/null || break
  sleep 0.05
done
kill -0 "$daemon_pid" 2>/dev/null && fail 'daemon did not stop'
daemon_pid=''
[[ "$(run_ilmari daemon status)" == 'stopped' ]] || fail 'daemon status did not become stopped'
[[ -z "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_window_badges)" ]] \
  || fail 'badge fragment was not cleaned'
[[ -z "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_status_summary)" ]] \
  || fail 'status fragment was not cleaned'

printf '%s\n' 'tmux daemon integration: ok'
