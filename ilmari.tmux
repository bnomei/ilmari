#!/usr/bin/env bash
# TPM entrypoint for Ilmari.
#
# Adds a tmux popup binding and manages the optional per-server Ilmari daemon.
# It publishes no layout or theme formats of its own.

set -euo pipefail

# TPM invokes plugin entrypoints from tmux, so TMUX is the authoritative server
# identity. Keep every tmux call pinned to that socket; never inherit a different
# server selected by a later shell environment.
if [[ -z "${TMUX:-}" ]]; then
  printf '%s\n' 'ilmari.tmux: TMUX is not set; run this entrypoint from tmux' >&2
  exit 1
fi

tmux_without_client="${TMUX%,*}"
tmux_client_id="${TMUX##*,}"
tmux_socket="${tmux_without_client%,*}"
tmux_server_pid="${tmux_without_client##*,}"
if [[ "$tmux_without_client" == "$TMUX" || "$tmux_socket" == "$tmux_without_client" || -z "$tmux_socket" ]]; then
  printf '%s\n' 'ilmari.tmux: could not parse the current tmux context' >&2
  exit 1
fi
case "$tmux_server_pid" in
  ''|*[!0-9]*)
    printf '%s\n' 'ilmari.tmux: tmux server PID is not numeric' >&2
    exit 1
    ;;
esac
case "$tmux_client_id" in
  ''|*[!0-9]*)
    printf '%s\n' 'ilmari.tmux: tmux client id is not numeric' >&2
    exit 1
    ;;
esac

tmux_cmd=(tmux -S "$tmux_socket")
tmux_identity="$("${tmux_cmd[@]}" display-message -p "#{socket_path}	#{pid}")"
case "$tmux_identity" in
  *$'\t'*) ;;
  *)
    printf '%s\n' 'ilmari.tmux: could not verify the current tmux server identity' >&2
    exit 1
    ;;
esac
responding_tmux_socket="${tmux_identity%$'\t'*}"
responding_tmux_server_pid="${tmux_identity##*$'\t'}"
if [[ "$responding_tmux_server_pid" != "$tmux_server_pid" ]]; then
  printf '%s\n' 'ilmari.tmux: the originating tmux server generation changed' >&2
  exit 1
fi
tmux_socket="$responding_tmux_socket"
tmux_cmd=(tmux -S "$tmux_socket")
tmux_context="$tmux_socket,$tmux_server_pid,$tmux_client_id"

socket_file_identity() {
  stat -f '%d:%i' -- "$1" 2>/dev/null || stat -c '%d:%i' -- "$1" 2>/dev/null
}

tmux_socket_file_identity="$(socket_file_identity "$tmux_socket")"
if [[ -z "$tmux_socket_file_identity" ]]; then
  printf '%s\n' 'ilmari.tmux: could not bind the originating tmux socket identity' >&2
  exit 1
fi

tmux_format_literal_into() {
  local destination="$1"
  local value="$2"
  value="${value//#/##}"
  value="${value//,/#,}"
  value="${value//\}/#\}}"
  printf -v "$destination" '%s' "$value"
}

shell_quote_into() {
  local destination="$1"
  local value="${2//\'/\'\\\'\'}"
  printf -v "$destination" "'%s'" "$value"
}

# Quote one value for tmux's second command-list parse. Values remain a single
# argument even when they contain option syntax, semicolons, formats, or lines.
tmux_quote() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//\$/\\\$}"
  value="${value//;/\\;}"
  value="${value//#/\\#}"
  value="${value//$'\n'/\\n}"
  value="${value//$'\r'/\\r}"
  value="${value//$'\t'/\\t}"
  printf '"%s"' "$value"
}

tmux_command_list() {
  local result='' argument quoted
  for argument in "$@"; do
    quoted="$(tmux_quote "$argument")"
    result+="${result:+ }$quoted"
  done
  printf '%s' "$result"
}

append_tmux_command() {
  local command_list
  command_list="$(tmux_command_list "$@")"
  tmux_action+="${tmux_action:+ ; }$command_list"
}

tmux_format_literal_into tmux_socket_format "$tmux_socket"
shell_quote_into tmux_socket_stat_shell "$tmux_socket_format"
tmux_guard_format="#{&&:#{==:#{pid},$tmux_server_pid},#{==:#{socket_path},$tmux_socket_format}}"
tmux_guard_condition="test x$tmux_guard_format = x1 && ilmari_socket_identity=\$(stat -f '%d:%i' -- $tmux_socket_stat_shell 2>/dev/null || stat -c '%d:%i' -- $tmux_socket_stat_shell 2>/dev/null) && test x\"\$ilmari_socket_identity\" = x'$tmux_socket_file_identity'"
tmux_guard_accepted='__ILMARI_TMUX_GENERATION_ACCEPTED__'
tmux_guard_rejected='__ILMARI_TMUX_GENERATION_REJECTED__'
tmux_guard_end='__ILMARI_TMUX_COMMAND_OUTPUT_END__'
tmux_last_output=''

tmux_guarded_raw() {
  local action="$1" accepted rejected output first_line body
  tmux_last_output=''
  if [[ "$(socket_file_identity "$tmux_socket" 2>/dev/null || true)" != "$tmux_socket_file_identity" ]]; then
    printf '%s\n' 'ilmari.tmux: the originating tmux socket identity changed' >&2
    return 1
  fi
  accepted="$(tmux_command_list display-message -p "$tmux_guard_accepted") ; $action ; $(tmux_command_list display-message -p "$tmux_guard_end")"
  rejected="$(tmux_command_list display-message -p "$tmux_guard_rejected")"
  output="$("${tmux_cmd[@]}" if-shell "$tmux_guard_condition" "$accepted" "$rejected")" || return 1
  first_line="${output%%$'\n'*}"
  if [[ "$first_line" != "$tmux_guard_accepted" ]]; then
    printf '%s\n' 'ilmari.tmux: the originating tmux server generation changed' >&2
    return 1
  fi
  [[ "$output" == *$'\n'* ]] || return 1
  body="${output#*$'\n'}"
  if [[ "$body" == "$tmux_guard_end" ]]; then
    tmux_last_output=''
  elif [[ "$body" == *$'\n'"$tmux_guard_end" ]]; then
    tmux_last_output="${body%$'\n'"$tmux_guard_end"}"
  else
    printf '%s\n' 'ilmari.tmux: guarded tmux output terminator was missing' >&2
    return 1
  fi
}

tmux_guarded() {
  local action
  action="$(tmux_command_list "$@")"
  tmux_guarded_raw "$action"
}

get_tmux_option_into() {
  local destination="$1"
  local option="$2"
  local default_value="$3"
  local value

  tmux_guarded show-option -gqv "$option" 2>/dev/null || return 1
  value="$tmux_last_output"
  if [[ -n "$value" ]]; then
    printf -v "$destination" '%s' "$value"
  else
    printf -v "$destination" '%s' "$default_value"
  fi
}

clear_published_state() {
  local snapshot_owner="$1"
  local snapshot_socket="$2"
  local snapshot_mcp="$3"
  local pane_id tmux_action='' role_condition ownerless_condition
  local alias_condition claim_condition clear_alias scratch_cleanup owned_cleanup
  local ownerless_cleanup cleanup_decision snapshot_owner_value snapshot_socket_value
  local expected_owner expected_socket

  role_condition="#{&&:#{==:x#{@ilmari_daemon_owner_pid},#{${snapshot_owner}}},#{==:x#{@ilmari_daemon_socket_path},#{${snapshot_socket}}},#{==:x#{@ilmari_daemon_mcp_url},#{${snapshot_mcp}}}}"
  ownerless_condition="#{==:#{${snapshot_owner}},x}"

  build_daemon_render_cleanup
  alias_condition="#{==:x#{@ilmari_socket_path},#{${snapshot_socket}}}"
  claim_condition="#{==:x#{@ilmari_daemon_legacy_socket_path},#{${snapshot_socket}}}"
  clear_alias="$(tmux_command_list if-shell -F "$alias_condition" "$(tmux_command_list set-option -gu '@ilmari_socket_path')" '') ; $(tmux_command_list set-option -gu '@ilmari_daemon_legacy_socket_path')"
  append_tmux_command if-shell -F "$claim_condition" "$clear_alias" ''
  append_tmux_command set-option -gu '@ilmari_daemon_socket_path'

  alias_condition="#{==:x#{@ilmari_mcp_url},#{${snapshot_mcp}}}"
  claim_condition="#{==:x#{@ilmari_daemon_legacy_mcp_url},#{${snapshot_mcp}}}"
  clear_alias="$(tmux_command_list if-shell -F "$alias_condition" "$(tmux_command_list set-option -gu '@ilmari_mcp_url')" '') ; $(tmux_command_list set-option -gu '@ilmari_daemon_legacy_mcp_url')"
  append_tmux_command if-shell -F "$claim_condition" "$clear_alias" ''
  append_tmux_command set-option -gu '@ilmari_daemon_mcp_url'
  append_tmux_command set-option -gu "$snapshot_owner"
  append_tmux_command set-option -gu "$snapshot_socket"
  append_tmux_command set-option -gu "$snapshot_mcp"
  # Owner removal is literally the final command in an accepted owned cleanup.
  append_tmux_command set-option -gu '@ilmari_daemon_owner_pid'
  owned_cleanup="$(tmux_command_list if-shell -F "$role_condition" "$tmux_action" "$(snapshot_cleanup_list "$snapshot_owner" "$snapshot_socket" "$snapshot_mcp")")"

  tmux_action=''
  build_daemon_render_cleanup
  append_tmux_command set-option -gu "$snapshot_owner"
  append_tmux_command set-option -gu "$snapshot_socket"
  append_tmux_command set-option -gu "$snapshot_mcp"
  ownerless_cleanup="$(tmux_command_list if-shell -F '#{==:#{@ilmari_daemon_owner_pid},}' "$tmux_action" "$(snapshot_cleanup_list "$snapshot_owner" "$snapshot_socket" "$snapshot_mcp")")"

  scratch_cleanup="$(snapshot_cleanup_list "$snapshot_owner" "$snapshot_socket" "$snapshot_mcp")"
  tmux_guarded show-option -gqv "$snapshot_owner" 2>/dev/null || return 1
  snapshot_owner_value="$tmux_last_output"
  tmux_guarded show-option -gqv "$snapshot_socket" 2>/dev/null || return 1
  snapshot_socket_value="$tmux_last_output"
  expected_owner="${snapshot_owner_value#x}"
  expected_socket="${snapshot_socket_value#x}"
  case "$expected_owner" in
    ''|*[!0-9]*) ;;
    *)
      if [[ -S "$expected_socket" ]] && kill -0 "$expected_owner" 2>/dev/null; then
        tmux_guarded_raw "$scratch_cleanup" 2>/dev/null || true
        return 0
      fi
      ;;
  esac
  cleanup_decision="$(tmux_command_list if-shell -F "$ownerless_condition" "$ownerless_cleanup" "$owned_cleanup")"
  tmux_guarded_raw "$cleanup_decision" 2>/dev/null || true
}

snapshot_cleanup_list() {
  local owner="$1" socket="$2" mcp="$3" result
  result="$(tmux_command_list set-option -gu "$owner")"
  result+=" ; $(tmux_command_list set-option -gu "$socket")"
  result+=" ; $(tmux_command_list set-option -gu "$mcp")"
  printf '%s' "$result"
}

build_daemon_render_cleanup() {
  local option pane_id
  local global_options=(
    '@ilmari_window_badges'
    '@ilmari_status_summary'
    '@ilmari_running_count'
    '@ilmari_waiting_count'
    '@ilmari_finished_count'
  )

  for option in "${global_options[@]}"; do
    append_tmux_command set-option -gu "$option"
  done

  if tmux_guarded list-panes -a -F '#{pane_id}' 2>/dev/null; then
    pane_ids="$tmux_last_output"
  else
    pane_ids=''
  fi
  while IFS= read -r pane_id; do
    [[ -n "$pane_id" ]] || continue
    append_tmux_command set-option -pqu -t "$pane_id" '@ilmari_state'
    append_tmux_command set-option -pqu -t "$pane_id" '@ilmari_badge'
  done <<<"$pane_ids"
}

start_daemon() {
  # The command is deliberately interpreted as a shell command because the tmux
  # option may include an absolute binary path and arguments. Quoting it as one
  # `sh -c` argument keeps it isolated from this entrypoint's shell.
  TMUX="$tmux_context" nohup sh -c "$ilmari_daemon_command" </dev/null >/dev/null 2>&1 &
}

stop_daemon() {
  local snapshot_prefix snapshot_owner snapshot_socket snapshot_mcp snapshot_action

  # The paired stop command is explicit because start commands may contain
  # wrappers or arbitrary arguments that cannot be reversed safely. Cleanup is
  # also done here so disabling the plugin works with an older or dead binary.
  snapshot_prefix="@ilmari_cleanup_${$}_${tmux_client_id}"
  snapshot_owner="${snapshot_prefix}_owner"
  snapshot_socket="${snapshot_prefix}_socket"
  snapshot_mcp="${snapshot_prefix}_mcp"
  snapshot_action="$(tmux_command_list set-option -gqF "$snapshot_owner" 'x#{@ilmari_daemon_owner_pid}')"
  snapshot_action+=" ; $(tmux_command_list set-option -gqF "$snapshot_socket" 'x#{@ilmari_daemon_socket_path}')"
  snapshot_action+=" ; $(tmux_command_list set-option -gqF "$snapshot_mcp" 'x#{@ilmari_daemon_mcp_url}')"
  tmux_guarded_raw "$snapshot_action" 2>/dev/null || return 1
  TMUX="$tmux_context" sh -c "$ilmari_daemon_stop_command" </dev/null >/dev/null 2>&1 || true
  clear_published_state "$snapshot_owner" "$snapshot_socket" "$snapshot_mcp"
}

truthy() {
  case "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" in
    1|yes|true|on) return 0 ;;
    *) return 1 ;;
  esac
}

get_tmux_option_into ilmari_key '@ilmari_key' 'i'
get_tmux_option_into ilmari_command '@ilmari_command' 'ilmari'
get_tmux_option_into ilmari_popup_width '@ilmari_popup_width' '90%'
get_tmux_option_into ilmari_popup_height '@ilmari_popup_height' '85%'
get_tmux_option_into ilmari_popup_extra '@ilmari_popup_extra' ''
get_tmux_option_into ilmari_bind_key '@ilmari_bind_key' 'on'
get_tmux_option_into ilmari_daemon '@ilmari_daemon' 'on'
get_tmux_option_into ilmari_daemon_command '@ilmari_daemon_command' 'ilmari daemon start'
get_tmux_option_into ilmari_daemon_stop_command '@ilmari_daemon_stop_command' 'ilmari daemon stop'

if truthy "$ilmari_daemon"; then
  start_daemon
else
  stop_daemon
fi

if truthy "$ilmari_bind_key"; then
  popup_extra_args=()
  if [[ -n "$ilmari_popup_extra" ]]; then
    read -r -a popup_extra_args <<<"$ilmari_popup_extra"
  fi
  # Keep the array nonempty before expanding it: Bash 3.2 treats an empty
  # "${array[@]}" expansion as an unbound variable under nounset.
  popup_extra_args+=("$ilmari_command")
  tmux_guarded bind-key "$ilmari_key" display-popup -E -w "$ilmari_popup_width" \
    -h "$ilmari_popup_height" "${popup_extra_args[@]}"
fi
