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

tmux_socket="${TMUX%%,*}"
if [[ -z "$tmux_socket" ]]; then
  printf '%s\n' 'ilmari.tmux: could not determine the current tmux socket' >&2
  exit 1
fi

tmux_cmd=(tmux -S "$tmux_socket")
tmux_socket="$("${tmux_cmd[@]}" display-message -p '#{socket_path}')"
tmux_cmd=(tmux -S "$tmux_socket")
tmux_context="$tmux_socket,${TMUX#*,}"

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
  local option pane_id
  local global_options=(
    '@ilmari_window_badges'
    '@ilmari_status_summary'
    '@ilmari_running_count'
    '@ilmari_waiting_count'
    '@ilmari_finished_count'
    '@ilmari_socket_path'
    '@ilmari_mcp_url'
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
  local daemon_stop_command

  # `daemon stop` uses TMUX to find the same per-server socket as the process
  # started above. Cleanup is also done here so disabling the plugin works with
  # an older or already-dead binary.
  if [[ "$ilmari_daemon_command" == *' daemon start' ]]; then
    daemon_stop_command="${ilmari_daemon_command% daemon start} daemon stop"
  else
    daemon_stop_command='ilmari daemon stop'
  fi
  TMUX="$tmux_context" sh -c "$daemon_stop_command" </dev/null >/dev/null 2>&1 || true
  clear_published_state
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
