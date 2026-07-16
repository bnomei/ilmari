#!/usr/bin/env bash
# Isolated lifecycle coverage for ilmari.tmux. No user tmux server is touched.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ilmari-tmux-plugin.XXXXXX")"
tmux_socket="$tmp_dir/tmux.sock"
test_log="$tmp_dir/ilmari.log"

cleanup() {
  tmux -S "$tmux_socket" kill-server 2>/dev/null || true
  rm -rf "$tmp_dir"
}
trap cleanup EXIT INT TERM

fail() {
  printf 'tmux plugin integration: %s\n' "$*" >&2
  exit 1
}

read_popup_binding() {
  tmux -S "$tmux_socket" list-keys -T prefix \
    | awk '$1 == "bind-key" && $2 == "-T" && $3 == "prefix" && $4 == "I" { gsub(/"/, ""); print }'
}

mkdir -p "$tmp_dir/bin"
cat >"$tmp_dir/bin/ilmari" <<'FAKE_ILMARI'
#!/usr/bin/env bash
set -euo pipefail
printf '%s|%s\n' "${TMUX:-}" "$*" >>"$ILMARI_TEST_LOG"
FAKE_ILMARI
chmod +x "$tmp_dir/bin/ilmari"

tmux -S "$tmux_socket" new-session -d -s ilmari-test 'sleep 120'
server_pid="$(tmux -S "$tmux_socket" display-message -p '#{pid}')"
tmux_context="$tmux_socket,$server_pid,0"
pane_id="$(tmux -S "$tmux_socket" display-message -p '#{pane_id}')"

tmux -S "$tmux_socket" set-option -g window-status-format 'KEEP-WINDOW'
tmux -S "$tmux_socket" set-option -g window-status-current-format 'KEEP-CURRENT'
tmux -S "$tmux_socket" set-option -g status-left 'KEEP-LEFT'
tmux -S "$tmux_socket" set-option -g status-right 'KEEP-RIGHT'
tmux -S "$tmux_socket" set-option -g @ilmari_key 'I'
tmux -S "$tmux_socket" set-option -g @ilmari_command 'ilmari --no-git'
tmux -S "$tmux_socket" set-option -g @ilmari_popup_width '81%'
tmux -S "$tmux_socket" set-option -g @ilmari_popup_height '73%'
tmux -S "$tmux_socket" set-option -g @ilmari_popup_extra ''
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_command 'ilmari daemon start'

PATH="$tmp_dir/bin:$PATH" ILMARI_TEST_LOG="$test_log" TMUX="$tmux_context" \
  bash "$repo_root/ilmari.tmux"

for _ in {1..50}; do
  [[ -s "$test_log" ]] && break
  sleep 0.02
done
[[ -s "$test_log" ]] || fail 'background daemon command did not run'
grep -F "$tmux_context|daemon start" "$test_log" >/dev/null \
  || fail 'daemon start was not routed through the originating TMUX context'

binding="$(read_popup_binding)"
[[ "$binding" == *'display-popup'* ]] || fail 'popup binding was not installed'
[[ "$binding" == *'-w 81%'* ]] || fail 'popup width option was not preserved'
[[ "$binding" == *'-h 73%'* ]] || fail 'popup height option was not preserved'
[[ "$binding" == *'ilmari --no-git'* ]] || fail 'popup command option was not preserved'

# Reload with multiple extra popup arguments to cover deterministic word
# boundaries as well as the empty default exercised above.
tmux -S "$tmux_socket" set-option -g @ilmari_popup_extra '-x 17 -y 9'
PATH="$tmp_dir/bin:$PATH" ILMARI_TEST_LOG="$test_log" TMUX="$tmux_context" \
  bash "$repo_root/ilmari.tmux"

binding="$(read_popup_binding)"
[[ "$binding" == *'-x 17'* ]] || fail 'popup extra x argument was not preserved'
[[ "$binding" == *'-y 9'* ]] || fail 'popup extra y argument was not preserved'
[[ "$binding" == *'ilmari --no-git'* ]] || fail 'popup command was not kept after popup extras'

# Simulate daemon-published state, then disable daemon management and reload.
for option in \
  @ilmari_window_badges \
  @ilmari_status_summary \
  @ilmari_running_count \
  @ilmari_waiting_count \
  @ilmari_finished_count \
  @ilmari_socket_path \
  @ilmari_mcp_url; do
  tmux -S "$tmux_socket" set-option -g "$option" 'stale'
done
tmux -S "$tmux_socket" set-option -p -t "$pane_id" @ilmari_state 'running'
tmux -S "$tmux_socket" set-option -p -t "$pane_id" @ilmari_badge 'R'
tmux -S "$tmux_socket" set-option -g @ilmari_badges_enabled 'off'
tmux -S "$tmux_socket" set-option -g @ilmari_status_enabled 'off'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon 'off'

PATH="$tmp_dir/bin:$PATH" ILMARI_TEST_LOG="$test_log" TMUX="$tmux_context" \
  bash "$repo_root/ilmari.tmux"

grep -F "$tmux_context|daemon stop" "$test_log" >/dev/null \
  || fail 'daemon stop was not routed through the originating TMUX context'

for option in \
  @ilmari_window_badges \
  @ilmari_status_summary \
  @ilmari_running_count \
  @ilmari_waiting_count \
  @ilmari_finished_count \
  @ilmari_socket_path \
  @ilmari_mcp_url; do
  [[ -z "$(tmux -S "$tmux_socket" show-option -gqv "$option")" ]] \
    || fail "$option was not cleared"
done
[[ -z "$(tmux -S "$tmux_socket" show-option -pqv -t "$pane_id" @ilmari_state)" ]] \
  || fail '@ilmari_state was not cleared'
[[ -z "$(tmux -S "$tmux_socket" show-option -pqv -t "$pane_id" @ilmari_badge)" ]] \
  || fail '@ilmari_badge was not cleared'

# Renderer enable overrides are user controls, not published state.
[[ "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_badges_enabled)" == 'off' ]] \
  || fail '@ilmari_badges_enabled should be preserved'
[[ "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_status_enabled)" == 'off' ]] \
  || fail '@ilmari_status_enabled should be preserved'

[[ "$(tmux -S "$tmux_socket" show-option -gv window-status-format)" == 'KEEP-WINDOW' ]] \
  || fail 'window-status-format was modified'
[[ "$(tmux -S "$tmux_socket" show-option -gv window-status-current-format)" == 'KEEP-CURRENT' ]] \
  || fail 'window-status-current-format was modified'
[[ "$(tmux -S "$tmux_socket" show-option -gv status-left)" == 'KEEP-LEFT' ]] \
  || fail 'status-left was modified'
[[ "$(tmux -S "$tmux_socket" show-option -gv status-right)" == 'KEEP-RIGHT' ]] \
  || fail 'status-right was modified'

printf '%s\n' 'tmux plugin integration: ok'
