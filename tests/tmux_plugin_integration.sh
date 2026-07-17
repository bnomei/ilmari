#!/usr/bin/env bash
# Isolated lifecycle coverage for ilmari.tmux. No user tmux server is touched.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ilmari,tmux-plugin.XXXXXX")"
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
case "$*" in
  'daemon start'*|'daemon stop'*)
    case "${ILMARI_TMUX_ORIGIN_SOCKET_DEVICE:-}" in
      ''|*[!0-9]*) exit 95 ;;
    esac
    case "${ILMARI_TMUX_ORIGIN_SOCKET_INODE:-}" in
      ''|*[!0-9]*) exit 95 ;;
    esac
    [[ "${ILMARI_TMUX_ORIGIN_SOCKET_DEVICE}:${ILMARI_TMUX_ORIGIN_SOCKET_INODE}" \
      == "${ILMARI_EXPECTED_SOCKET_IDENTITY:-}" ]] || exit 94
    ;;
esac
printf '%s|%s\n' "${TMUX:-}" "$*" >>"$ILMARI_TEST_LOG"
FAKE_ILMARI
chmod +x "$tmp_dir/bin/ilmari"

mkdir -p "$tmp_dir/replacement-bin"
real_tmux="$(command -v tmux)"
cat >"$tmp_dir/replacement-bin/tmux" <<'FAKE_TMUX'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${3:-}" == 'display-message' && "${4:-}" == '-p' \
  && "${5:-}" == *'#{socket_path}'* ]]; then
  exec "$REAL_TMUX" "$@"
fi
if [[ "${3:-}" == 'if-shell' ]]; then
  if [[ -n "${ILMARI_REJECT_GUARDED_MATCH:-}" ]]; then
    if [[ "$*" == *"$ILMARI_REJECT_GUARDED_MATCH"* ]]; then
      if [[ "${ILMARI_REQUIRE_STOP_SNAPSHOT:-}" == '1' \
        && "$*" != *'@ilmari_cleanup_'* ]]; then
        exit 96
      fi
      printf '%s\n' '__ILMARI_TMUX_GENERATION_REJECTED__'
      exit 0
    fi
    exec "$REAL_TMUX" "$@"
  fi
  printf '%s\n' '__ILMARI_TMUX_GENERATION_REJECTED__'
  exit 0
fi
exit 97
FAKE_TMUX
chmod +x "$tmp_dir/replacement-bin/tmux"

tmux -S "$tmux_socket" new-session -d -s ilmari-test 'sleep 120'
server_pid="$(tmux -S "$tmux_socket" display-message -p '#{pid}')"
tmux_context="$tmux_socket,$server_pid,0"
pane_id="$(tmux -S "$tmux_socket" display-message -p '#{pane_id}')"
expected_socket_identity="$(
  stat -f '%d:%i' -- "$tmux_socket" 2>/dev/null \
    || stat -c '%d:%i' -- "$tmux_socket" 2>/dev/null
)"
tmux -S "$tmux_socket" set-environment -g PATH "$tmp_dir/bin:$PATH"
tmux -S "$tmux_socket" set-environment -g ILMARI_TEST_LOG "$test_log"
tmux -S "$tmux_socket" set-environment -g ILMARI_EXPECTED_SOCKET_IDENTITY \
  "$expected_socket_identity"

# A process launched by a replaced server must fail closed before it starts or
# stops anything, even when the replacement reuses the same socket pathname.
if PATH="$tmp_dir/bin:$PATH" ILMARI_TEST_LOG="$test_log" \
  TMUX="$tmux_socket,$((server_pid + 1)),0" bash "$repo_root/ilmari.tmux" 2>/dev/null; then
  fail 'TPM accepted a responding tmux server with a different originating PID'
fi
[[ ! -e "$test_log" ]] || fail 'generation mismatch executed an Ilmari command'

# Deterministically replace the responding generation after the startup probe:
# the guarded action is rejected by the server-side branch, and no configured
# command or binding may be accepted from the stale plugin process.
if PATH="$tmp_dir/replacement-bin:$tmp_dir/bin:$PATH" REAL_TMUX="$real_tmux" \
  ILMARI_TEST_LOG="$test_log" TMUX="$tmux_context" \
  bash "$repo_root/ilmari.tmux" 2>/dev/null; then
  fail 'TPM accepted an action after replacement between startup probe and action'
fi
[[ ! -e "$test_log" ]] || fail 'post-probe replacement executed an Ilmari command'

tmux -S "$tmux_socket" set-option -g window-status-format 'KEEP-WINDOW'
tmux -S "$tmux_socket" set-option -g window-status-current-format 'KEEP-CURRENT'
tmux -S "$tmux_socket" set-option -g status-left 'KEEP-LEFT'
tmux -S "$tmux_socket" set-option -g status-right 'KEEP-RIGHT'
tmux -S "$tmux_socket" set-option -g @ilmari_key 'I'
tmux -S "$tmux_socket" set-option -g @ilmari_command 'ilmari --no-git'
tmux -S "$tmux_socket" set-option -g @ilmari_popup_width '81%'
tmux -S "$tmux_socket" set-option -g @ilmari_popup_height '73%'
tmux -S "$tmux_socket" set-option -g @ilmari_popup_extra ''

# All configured-option reads may succeed, but replacing the generation before
# the receiving server accepts the start action must not launch the command.
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_command \
  'ilmari daemon start --must-not-start late-rejection'
: >"$test_log"
if PATH="$tmp_dir/replacement-bin:$tmp_dir/bin:$PATH" REAL_TMUX="$real_tmux" \
  ILMARI_REJECT_GUARDED_MATCH='daemon start --must-not-start late-rejection' \
  ILMARI_TEST_LOG="$test_log" TMUX="$tmux_context" \
  bash "$repo_root/ilmari.tmux" 2>/dev/null; then
  fail 'TPM accepted a configured start after the final option read'
fi
[[ ! -s "$test_log" ]] || fail 'rejected configured start command executed'

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

# The stop option has a complete default contract paired with the default start
# command; it does not need to be present in tmux configuration.
: >"$test_log"
tmux -S "$tmux_socket" set-option -g @ilmari_daemon 'off'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_stop_command \
  'ilmari daemon stop --must-not-stop late-rejection'
if PATH="$tmp_dir/replacement-bin:$tmp_dir/bin:$PATH" REAL_TMUX="$real_tmux" \
  ILMARI_REJECT_GUARDED_MATCH='daemon stop --must-not-stop late-rejection' \
  ILMARI_REQUIRE_STOP_SNAPSHOT=1 ILMARI_TEST_LOG="$test_log" TMUX="$tmux_context" \
  bash "$repo_root/ilmari.tmux" 2>/dev/null; then
  fail 'TPM accepted a configured stop after the final option read'
fi
[[ ! -s "$test_log" ]] || fail 'rejected configured stop command executed'
tmux -S "$tmux_socket" set-option -gu @ilmari_daemon_stop_command

PATH="$tmp_dir/bin:$PATH" ILMARI_TEST_LOG="$test_log" TMUX="$tmux_context" \
  bash "$repo_root/ilmari.tmux"
grep -F "$tmux_context|daemon stop" "$test_log" >/dev/null \
  || fail 'default daemon stop command was not used'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon 'on'

# Custom daemon start commands may contain arguments that cannot be reversed
# reliably. The explicitly paired stop command must be used verbatim.
: >"$test_log"
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_command \
  'ilmari daemon start --refresh-seconds 11'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_stop_command \
  'ilmari daemon stop --custom-stop-marker paired'
PATH="$tmp_dir/bin:$PATH" ILMARI_TEST_LOG="$test_log" TMUX="$tmux_context" \
  bash "$repo_root/ilmari.tmux"

for _ in {1..50}; do
  grep -F "$tmux_context|daemon start --refresh-seconds 11" "$test_log" >/dev/null 2>&1 \
    && break
  sleep 0.02
done
grep -F "$tmux_context|daemon start --refresh-seconds 11" "$test_log" >/dev/null \
  || fail 'custom daemon start command with arguments was not used verbatim'

# Replacement during the paired stop command must invalidate the old cleanup
# tuple before the single destructive tmux phase is accepted.
cat >"$tmp_dir/bin/replace-ilmari-daemon" <<'REPLACE_DAEMON'
#!/usr/bin/env bash
set -euo pipefail
without_client="${TMUX%,*}"
socket="${without_client%,*}"
tmux -S "$socket" set-option -g @ilmari_daemon_owner_pid '4343'
tmux -S "$socket" set-option -g @ilmari_daemon_socket_path '/tmp/replacement.sock'
tmux -S "$socket" set-option -g @ilmari_daemon_mcp_url 'http://127.0.0.1:4030/mcp'
tmux -S "$socket" set-option -g @ilmari_window_badges 'replacement-render'
tmux -S "$socket" set-option -g @ilmari_status_summary 'replacement-status'
tmux -S "$socket" set-option -p @ilmari_state 'replacement-running'
tmux -S "$socket" set-option -p @ilmari_badge 'N'
REPLACE_DAEMON
chmod +x "$tmp_dir/bin/replace-ilmari-daemon"
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_owner_pid '4242'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_socket_path '/tmp/daemon.sock'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_mcp_url 'http://127.0.0.1:4010/mcp'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_stop_command 'replace-ilmari-daemon'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon 'off'
PATH="$tmp_dir/bin:$PATH" ILMARI_TEST_LOG="$test_log" TMUX="$tmux_context" \
  bash "$repo_root/ilmari.tmux"
[[ "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_daemon_owner_pid)" == '4343' ]] \
  || fail 'interleaved replacement owner was cleared'
[[ "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_daemon_socket_path)" == '/tmp/replacement.sock' ]] \
  || fail 'interleaved replacement endpoint was cleared'
[[ "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_window_badges)" == 'replacement-render' ]] \
  || fail 'interleaved replacement render state was cleared'
[[ "$(tmux -S "$tmux_socket" show-option -pqv -t "$pane_id" @ilmari_state)" == 'replacement-running' ]] \
  || fail 'interleaved replacement pane state was cleared'

# Restore the old tuple to cover successful owner cleanup below.
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_stop_command \
  'ilmari daemon stop --custom-stop-marker paired'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon 'on'

# Full set of daemon-published render globals (parity with Rust cleanup) plus
# daemon-role endpoints and pane-local state available in this slice.
render_global_options=(
  @ilmari_window_badges
  @ilmari_status_summary
  @ilmari_running_count
  @ilmari_waiting_count
  @ilmari_finished_count
  @ilmari_attention_count
  @ilmari_waiting_state_count
  @ilmari_finished_state_count
  @ilmari_terminated_count
  @ilmari_unknown_count
)
daemon_role_options=(
  @ilmari_daemon_socket_path
  @ilmari_daemon_owner_pid
  @ilmari_daemon_mcp_url
)

# Simulate daemon-published state alongside different popup-role legacy
# endpoints, then disable daemon management and reload.
for option in "${render_global_options[@]}"; do
  tmux -S "$tmux_socket" set-option -g "$option" 'stale'
done
tmux -S "$tmux_socket" set-option -g @ilmari_socket_path '/tmp/popup.sock'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_socket_path '/tmp/daemon.sock'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_owner_pid '4242'
tmux -S "$tmux_socket" set-option -g @ilmari_mcp_url 'http://127.0.0.1:4020/mcp'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon_mcp_url 'http://127.0.0.1:4010/mcp'
tmux -S "$tmux_socket" set-option -p -t "$pane_id" @ilmari_state 'running'
tmux -S "$tmux_socket" set-option -p -t "$pane_id" @ilmari_badge 'R'
tmux -S "$tmux_socket" set-option -p -t "$pane_id" @ilmari_attention '1'
tmux -S "$tmux_socket" set-option -g @ilmari_badges_enabled 'off'
tmux -S "$tmux_socket" set-option -g @ilmari_status_enabled 'off'
tmux -S "$tmux_socket" set-option -g @ilmari_daemon 'off'

PATH="$tmp_dir/bin:$PATH" ILMARI_TEST_LOG="$test_log" TMUX="$tmux_context" \
  bash "$repo_root/ilmari.tmux"

grep -F "$tmux_context|daemon stop --custom-stop-marker paired" "$test_log" >/dev/null \
  || fail 'explicit daemon stop command was not routed through the originating TMUX context'

for option in "${render_global_options[@]}" "${daemon_role_options[@]}"; do
  [[ -z "$(tmux -S "$tmux_socket" show-option -gqv "$option")" ]] \
    || fail "$option was not cleared"
done
[[ "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_socket_path)" == '/tmp/popup.sock' ]] \
  || fail 'popup-owned legacy socket publication was removed'
[[ "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_mcp_url)" == 'http://127.0.0.1:4020/mcp' ]] \
  || fail 'popup-owned legacy MCP publication was removed'
[[ -z "$(tmux -S "$tmux_socket" show-option -pqv -t "$pane_id" @ilmari_state)" ]] \
  || fail '@ilmari_state was not cleared'
[[ -z "$(tmux -S "$tmux_socket" show-option -pqv -t "$pane_id" @ilmari_badge)" ]] \
  || fail '@ilmari_badge was not cleared'
[[ -z "$(tmux -S "$tmux_socket" show-option -pqv -t "$pane_id" @ilmari_attention)" ]] \
  || fail '@ilmari_attention was not cleared'

# Older dead daemons have no owner or daemon-role endpoint metadata. Their
# renderer state is daemon-exclusive and must still be cleared, while ambiguous
# legacy socket/MCP publications remain untouched for a possible popup owner.
for option in "${render_global_options[@]}"; do
  tmux -S "$tmux_socket" set-option -g "$option" 'legacy-stale'
done
tmux -S "$tmux_socket" set-option -p -t "$pane_id" @ilmari_state 'waiting'
tmux -S "$tmux_socket" set-option -p -t "$pane_id" @ilmari_badge 'W'
tmux -S "$tmux_socket" set-option -p -t "$pane_id" @ilmari_attention '1'
tmux -S "$tmux_socket" set-option -gu @ilmari_daemon_owner_pid 2>/dev/null || true
tmux -S "$tmux_socket" set-option -gu @ilmari_daemon_socket_path 2>/dev/null || true
tmux -S "$tmux_socket" set-option -gu @ilmari_daemon_mcp_url 2>/dev/null || true

PATH="$tmp_dir/bin:$PATH" ILMARI_TEST_LOG="$test_log" TMUX="$tmux_context" \
  bash "$repo_root/ilmari.tmux"

for option in "${render_global_options[@]}"; do
  [[ -z "$(tmux -S "$tmux_socket" show-option -gqv "$option")" ]] \
    || fail "ownerless legacy $option was not cleared"
done
[[ -z "$(tmux -S "$tmux_socket" show-option -pqv -t "$pane_id" @ilmari_state)" ]] \
  || fail 'ownerless legacy @ilmari_state was not cleared'
[[ -z "$(tmux -S "$tmux_socket" show-option -pqv -t "$pane_id" @ilmari_badge)" ]] \
  || fail 'ownerless legacy @ilmari_badge was not cleared'
[[ -z "$(tmux -S "$tmux_socket" show-option -pqv -t "$pane_id" @ilmari_attention)" ]] \
  || fail 'ownerless legacy @ilmari_attention was not cleared'
[[ "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_socket_path)" == '/tmp/popup.sock' ]] \
  || fail 'ownerless cleanup removed ambiguous legacy socket publication'
[[ "$(tmux -S "$tmux_socket" show-option -gqv @ilmari_mcp_url)" == 'http://127.0.0.1:4020/mcp' ]] \
  || fail 'ownerless cleanup removed ambiguous legacy MCP publication'

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
