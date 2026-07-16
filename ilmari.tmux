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

get_tmux_option() {
  local option="$1"
  local default_value="$2"
  local value

  value="$("${tmux_cmd[@]}" show-option -gqv "$option" 2>/dev/null || true)"
  if [[ -n "$value" ]]; then
    printf '%s' "$value"
  else
    printf '%s' "$default_value"
  fi
}

clear_published_state() {
  local expected_owner_pid="$1"
  local expected_socket_path="$2"
  local expected_mcp_url="$3"
  local option current_owner_pid

  current_owner_pid="$("${tmux_cmd[@]}" show-option -gqv '@ilmari_daemon_owner_pid' 2>/dev/null || true)"
  if [[ -z "$expected_owner_pid" ]]; then
    # Older daemons published renderer state without ownership metadata. It is
    # safe to clear only daemon-exclusive badges/counts/pane state; legacy
    # socket and MCP options are ambiguous and may belong to a popup.
    [[ -z "$current_owner_pid" ]] || return 0
    clear_daemon_render_state
    return 0
  fi
  [[ "$current_owner_pid" == "$expected_owner_pid" ]] || return 0
  if [[ -n "$expected_socket_path" ]] \
    && [[ "$("${tmux_cmd[@]}" show-option -gqv '@ilmari_daemon_socket_path' 2>/dev/null || true)" != "$expected_socket_path" ]]; then
    return 0
  fi
  if [[ -n "$expected_mcp_url" ]] \
    && [[ "$("${tmux_cmd[@]}" show-option -gqv '@ilmari_daemon_mcp_url' 2>/dev/null || true)" != "$expected_mcp_url" ]]; then
    return 0
  fi
  # Do not take ownership away from a daemon that may still be performing its
  # own socket/MCP cleanup. Missing sockets and dead owner PIDs are safe
  # fallback cases; live owners retain their publication metadata.
  if [[ -n "$expected_socket_path" && -S "$expected_socket_path" ]] \
    && kill -0 "$expected_owner_pid" 2>/dev/null; then
    return 0
  fi

  clear_daemon_render_state

  if [[ -n "$expected_socket_path" ]]; then
    for option in '@ilmari_daemon_socket_path' '@ilmari_socket_path'; do
      if [[ "$("${tmux_cmd[@]}" show-option -gqv "$option" 2>/dev/null || true)" == "$expected_socket_path" ]]; then
        "${tmux_cmd[@]}" set-option -gu "$option" 2>/dev/null || true
      fi
    done
  fi
  if [[ -n "$expected_mcp_url" ]]; then
    for option in '@ilmari_daemon_mcp_url' '@ilmari_mcp_url'; do
      if [[ "$("${tmux_cmd[@]}" show-option -gqv "$option" 2>/dev/null || true)" == "$expected_mcp_url" ]]; then
        "${tmux_cmd[@]}" set-option -gu "$option" 2>/dev/null || true
      fi
    done
  fi
  if [[ "$("${tmux_cmd[@]}" show-option -gqv '@ilmari_daemon_owner_pid' 2>/dev/null || true)" == "$expected_owner_pid" ]]; then
    "${tmux_cmd[@]}" set-option -gu '@ilmari_daemon_owner_pid' 2>/dev/null || true
  fi
}

clear_daemon_render_state() {
  local option pane_id
  local global_options=(
    '@ilmari_window_badges'
    '@ilmari_status_summary'
    '@ilmari_running_count'
    '@ilmari_waiting_count'
    '@ilmari_finished_count'
  )

  for option in "${global_options[@]}"; do
    "${tmux_cmd[@]}" set-option -gu "$option" 2>/dev/null || true
  done

  while IFS= read -r pane_id; do
    [[ -n "$pane_id" ]] || continue
    "${tmux_cmd[@]}" set-option -pu -t "$pane_id" '@ilmari_state' 2>/dev/null || true
    "${tmux_cmd[@]}" set-option -pu -t "$pane_id" '@ilmari_badge' 2>/dev/null || true
  done < <("${tmux_cmd[@]}" list-panes -a -F '#{pane_id}' 2>/dev/null || true)
}

start_daemon() {
  # The command is deliberately interpreted as a shell command because the tmux
  # option may include an absolute binary path and arguments. Quoting it as one
  # `sh -c` argument keeps it isolated from this entrypoint's shell.
  TMUX="$tmux_context" nohup sh -c "$ilmari_daemon_command" </dev/null >/dev/null 2>&1 &
}

stop_daemon() {
  local daemon_owner_pid daemon_socket_path daemon_mcp_url

  # The paired stop command is explicit because start commands may contain
  # wrappers or arbitrary arguments that cannot be reversed safely. Cleanup is
  # also done here so disabling the plugin works with an older or dead binary.
  daemon_owner_pid="$("${tmux_cmd[@]}" show-option -gqv '@ilmari_daemon_owner_pid' 2>/dev/null || true)"
  daemon_socket_path="$("${tmux_cmd[@]}" show-option -gqv '@ilmari_daemon_socket_path' 2>/dev/null || true)"
  daemon_mcp_url="$("${tmux_cmd[@]}" show-option -gqv '@ilmari_daemon_mcp_url' 2>/dev/null || true)"
  TMUX="$tmux_context" sh -c "$ilmari_daemon_stop_command" </dev/null >/dev/null 2>&1 || true
  clear_published_state "$daemon_owner_pid" "$daemon_socket_path" "$daemon_mcp_url"
}

truthy() {
  case "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" in
    1|yes|true|on) return 0 ;;
    *) return 1 ;;
  esac
}

ilmari_key="$(get_tmux_option '@ilmari_key' 'i')"
ilmari_command="$(get_tmux_option '@ilmari_command' 'ilmari')"
ilmari_popup_width="$(get_tmux_option '@ilmari_popup_width' '90%')"
ilmari_popup_height="$(get_tmux_option '@ilmari_popup_height' '85%')"
ilmari_popup_extra="$(get_tmux_option '@ilmari_popup_extra' '')"
ilmari_bind_key="$(get_tmux_option '@ilmari_bind_key' 'on')"
ilmari_daemon="$(get_tmux_option '@ilmari_daemon' 'on')"
ilmari_daemon_command="$(get_tmux_option '@ilmari_daemon_command' 'ilmari daemon start')"
ilmari_daemon_stop_command="$(get_tmux_option '@ilmari_daemon_stop_command' 'ilmari daemon stop')"

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
  "${tmux_cmd[@]}" bind-key "$ilmari_key" display-popup -E -w "$ilmari_popup_width" -h "$ilmari_popup_height" "${popup_extra_args[@]}"
fi
