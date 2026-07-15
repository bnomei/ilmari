#!/usr/bin/env bash
# TPM entrypoint for Ilmari.
#
# Adds a tmux popup binding for the installed `ilmari` binary without owning the
# user's layout. Install with TPM, then press the configured key to open the radar.

set -euo pipefail

get_tmux_option() {
  local option="$1"
  local default_value="$2"
  local value

  value="$(tmux show-option -gqv "$option" 2>/dev/null || true)"
  if [[ -n "$value" ]]; then
    printf '%s' "$value"
  else
    printf '%s' "$default_value"
  fi
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

if truthy "$ilmari_bind_key"; then
  tmux bind-key "$ilmari_key" display-popup -E -w "$ilmari_popup_width" -h "$ilmari_popup_height" $ilmari_popup_extra "$ilmari_command"
fi
