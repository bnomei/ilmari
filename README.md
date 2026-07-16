# ilmari

[![Crates.io Version](https://img.shields.io/crates/v/ilmari)](https://crates.io/crates/ilmari)
[![CI](https://img.shields.io/github/actions/workflow/status/bnomei/ilmari/ci.yml?branch=main)](https://github.com/bnomei/ilmari/actions/workflows/ci.yml)
[![Crates.io Downloads](https://img.shields.io/crates/d/ilmari)](https://crates.io/crates/ilmari)
[![License](https://img.shields.io/crates/l/ilmari)](https://crates.io/crates/ilmari)
[![Discord](https://flat.badgen.net/badge/discord/bnomei?color=7289da&icon=discord&label)](https://discordapp.com/users/bnomei)
[![Buymecoffee](https://flat.badgen.net/badge/icon/donate?icon=buymeacoffee&color=FF813F&label)](https://www.buymeacoffee.com/bnomei)

Ilmari is a tmux popup radar for coding-agent panes.

It scans the tmux panes you already have running, detects supported agent CLIs,
groups panes by workspace, shows each pane's state and recent output, and jumps
you back to the selected pane. It is observer-only: it does not launch agents,
manage workflows, or own your tmux layout.

[![Ilmari tmux popup screenshot](https://raw.githubusercontent.com/bnomei/ilmari/main/screenshot.png)](https://raw.githubusercontent.com/bnomei/ilmari/main/screenshot.png)

## What it helps you answer

Use Ilmari when one tmux workspace has several agent sessions and you need to
answer these questions quickly:

- Which agent pane is still running?
- Which pane is waiting for input?
- Which workspace does that pane belong to?
- What did the pane print most recently?
- How do I jump back to it without cycling through panes manually?

Ilmari can also run one provider-neutral collector daemon per tmux server. The
daemon accelerates popup refreshes, publishes optional tmux badge/status
fragments, and serves the same read-only pane state through a local Unix JSON
socket or loopback MCP resource server.

## Supported agents

Ilmari enables detection for these agent CLIs:

The table lists the canonical commands. Some adapters also recognize wrapped,
remote, or title-based executions when tmux metadata, the process tree, or pane
output identifies an enabled agent.

| Agent | Commands |
| --- | --- |
| Antigravity CLI | `agy` |
| Gemini CLI | `gemini` |
| Codex | `codex` |
| Amp | `amp` |
| Claude Code | `claude` |
| OpenCode | `opencode` |
| Pi | `pi`, `pi-agent` |
| Auggie | `auggie` |
| Grok | `grok` |
| GitHub Copilot CLI | `copilot` |
| Kiro CLI | `kiro-cli` |

Tracked but disabled agent adapters:

| Agent | Tracking issue |
| --- | --- |
| Cursor CLI | [#11](https://github.com/bnomei/ilmari/issues/11) |
| Aider | [#12](https://github.com/bnomei/ilmari/issues/12) |
| Cline CLI | [#13](https://github.com/bnomei/ilmari/issues/13) |
| Goose CLI | [#14](https://github.com/bnomei/ilmari/issues/14) |
| OpenHands CLI | [#16](https://github.com/bnomei/ilmari/issues/16) |

## Platform support

Ilmari targets Unix-like tmux environments. Published release artifacts are
Linux and macOS binaries, and runtime usage assumes a Unix-like shell with
`tmux` available. Windows is currently out of scope because Windows tmux
behavior is not implemented or tested.

tmux popup usage requires tmux 3.2 or newer. Cargo builds require Rust 1.86.0
or newer.

## Quickstart

### Prerequisites

- `tmux` is installed and running.
- tmux is version 3.2 or newer if you want popup mode.
- At least one supported agent CLI is already running in a tmux pane.

### 1. Install Ilmari

```bash
cargo install ilmari
```

Verify the binary is on your `PATH`:

```bash
ilmari --version
```

Expected output:

```text
ilmari <version>
```


### TPM tmux plugin

If you manage tmux plugins with [TPM](https://github.com/tmux-plugins/tpm), you
can install Ilmari as a small tmux plugin that wires the popup binding for you.
The plugin still expects the `ilmari` binary to already be installed and visible
on tmux's `PATH`:

```tmux
set -g @plugin 'bnomei/ilmari'
```

Reload tmux and install the plugin with `prefix + I`. By default it binds
`prefix + i` to the same popup command shown in the quickstart and starts one
singleton daemon for the current tmux server. Configure these options before TPM
initializes plugins:

```tmux
set -g @ilmari_key 'I'
set -g @ilmari_command '/opt/homebrew/bin/ilmari --no-git'
set -g @ilmari_popup_width '90%'
set -g @ilmari_popup_height '85%'
set -g @ilmari_bind_key 'on'
set -g @ilmari_daemon 'on'
set -g @ilmari_daemon_command '/opt/homebrew/bin/ilmari daemon start'
```

| TPM option | Default | Purpose |
| --- | --- | --- |
| `@ilmari_key` | `i` | Prefix-table popup key. |
| `@ilmari_command` | `ilmari` | Command executed inside the popup. |
| `@ilmari_popup_width` | `90%` | Popup width. |
| `@ilmari_popup_height` | `85%` | Popup height. |
| `@ilmari_popup_extra` | empty | Additional whitespace-separated `display-popup` arguments. |
| `@ilmari_bind_key` | `on` | Whether the plugin installs the popup binding. |
| `@ilmari_daemon` | `on` | Whether this tmux server should run an Ilmari daemon. |
| `@ilmari_daemon_command` | `ilmari daemon start` | Foreground daemon command that the plugin starts in the background. Commands ending in `daemon start` use the same executable/wrapper for `daemon stop`. |

Set `@ilmari_bind_key` to `off` if you want TPM to install the plugin but prefer
to define your own binding. Set `@ilmari_daemon` to `off` to stop the daemon for
that tmux server and clear Ilmari's published pane/global state. Reloading with
the daemon enabled is safe: `daemon start` recognizes the healthy compatible
instance and exits successfully instead of starting a duplicate. The plugin
pins daemon lifecycle and all its own tmux commands to the socket of the server
that loaded it.

### 2. Add a tmux popup binding

Add this to `~/.tmux.conf`:

```tmux
bind-key i display-popup -E -w 90% -h 85% "ilmari"
```

Reload tmux:

```bash
tmux source-file ~/.tmux.conf
```

### 3. Open the radar

Press your tmux prefix, then `i`.

Typical flow:

1. Open `ilmari`.
2. Move the selection with `j` / `k` or the arrow keys.
3. Press `Enter` to jump to the selected pane.
4. Press `q`, `Esc`, or `Ctrl-C` to quit.

When Ilmari runs in a tmux popup, activating a pane returns you to that pane and
closes the popup. When it runs directly in a pane, activation switches to the
target pane and Ilmari keeps running.

## Installation

### Cargo

```bash
cargo install ilmari
```

### Homebrew

```bash
brew install bnomei/ilmari/ilmari
```

### GitHub Releases

Download a prebuilt Linux or macOS archive from
[GitHub Releases](https://github.com/bnomei/ilmari/releases), extract it, and
put `ilmari` on your `PATH`.

Windows archives are not published because Windows runtime behavior is not
supported.

### From source

```bash
git clone https://github.com/bnomei/ilmari.git
cd ilmari
cargo build --release
```

Verify the build:

```bash
target/release/ilmari --help
```

## Popup controls

| Key | Action |
| --- | --- |
| `j`, `Down` | Move to the next visible pane. |
| `k`, `Up` | Move to the previous visible pane. |
| `%`, then digits | Select a pane by tmux pane id. |
| `Enter` | Jump to the selected pane. |
| `q`, `Esc`, `Ctrl-C` | Quit. |
| `a` | Toggle the agent/app column. |
| `m` | Toggle the model/detail column. |
| `t` | Toggle inactive time. |
| `o` | Toggle recent output excerpts. |
| `g` | Toggle git summaries. |
| `s` | Toggle CPU and memory stats. |
| `R` | Clear remembered views and restore explicit TOML values or built-ins. |
| `=` | Expand or collapse subprocess stats for the selected pane. |
| `b` | Toggle terminal bell alerts. |

Ilmari sorts panes by workspace. Inside each workspace, running panes appear
first, waiting panes appear next, and finished, terminated, or unknown panes
follow. The most recent waiting pane is selected automatically when possible.

## What the radar shows

| Field | Description |
| --- | --- |
| Workspace | Derived from each pane's current path. |
| Status | `running`, `waiting-input`, `finished`, `terminated`, or `unknown`. |
| Pane id | Stable tmux pane id such as `%12`. |
| Agent | Detected agent kind, such as `Codex` or `Claude Code`. |
| Detail | Agent-specific model or mode label when the adapter can extract one. |
| Output | Recent, sanitized pane output excerpt when output-tail capture is enabled. |
| Git | Branch plus insertion/deletion summary for the workspace repository. |
| Stats | Agent and spawned subprocess CPU/memory samples when stats are visible. |

Terminal bell alerts fire when a pane transitions into a waiting-input or
finished state from a running, unknown, or retained terminated state. Use `b` or
`--no-bell` to disable them for the current run.

Ilmari applies the same detection and state model to every enabled agent adapter.
A window containing several supported agent panes can therefore display several
badges; there are no Codex-specific lifecycle hooks.

## Daemon and popup fallback

TPM starts the daemon by default, or you can manage it directly:

```bash
ilmari daemon start
ilmari daemon status
ilmari daemon stop
ilmari status
```

`daemon start` is a foreground headless collector with its Unix socket enabled;
service managers may supervise it directly. It is singleton-safe for the
originating tmux server/socket. `daemon stop` requests a clean shutdown, and
`daemon status` reports whether a compatible daemon is available. Signals and
stop requests clear published options while that tmux server still exists. A
daemon also exits after repeated proof that its target tmux server has gone.

On every refresh, the popup prefers one compatible full daemon snapshot that is
still within its advertised TTL. If the daemon is unavailable or its reply is
stale, malformed, or from an incompatible schema, the popup retries the existing
direct tmux scan and will try the daemon again later. If both sources fail after
a successful refresh, the last good rows stay visible with a warning instead of
being replaced by an empty display.

`ilmari status` prints the compact nonzero attention/running summary used by
command-based tmux status and menu helpers. It exits successfully without output
when the renderer is disabled or no daemon state is available.

## Tmux badges and status placement

Ilmari publishes format fragments but never edits `window-status-format`,
`window-status-current-format`, `status-left`, or `status-right`. Place the
fragments wherever they fit your theme, for example:

```tmux
set -ag window-status-format ' #{E:@ilmari_window_badges}'
set -ag window-status-current-format ' #{E:@ilmari_window_badges}'
set -ag status-right ' #{E:@ilmari_status_summary}'
```

`@ilmari_window_badges` can render more than one badge when a window contains
multiple agent panes. `@ilmari_status_summary` contains only nonzero counts, in
waiting-input, unacknowledged-finished, then running priority. The underlying
counts are also available as `@ilmari_waiting_count`, `@ilmari_finished_count`,
and `@ilmari_running_count`. Pane-local `@ilmari_state` and `@ilmari_badge`
options are available to advanced custom formats.

Running state is live. Waiting-input and finished attention is sticky only after
a qualifying transition occurs while that exact pane is not focused by any tmux
client. Focusing the pane acknowledges it. An unchanged later scan does not
recreate attention, but a later qualifying transition can. State for disappeared
panes is removed.

TOML supplies the default renderer enablement and symbols/styles. These tmux
server-local overrides are polled and applied immediately without restarting
the daemon:

```tmux
set -g @ilmari_badges_enabled 'off'
set -g @ilmari_status_enabled 'off'
```

A disabled renderer publishes an empty fragment; collection, snapshots, and
popup acceleration continue.

Before uninstalling the TPM plugin, either set `@ilmari_daemon off` and reload
tmux or run `ilmari daemon stop` from that tmux server. This stops its daemon and
clears Ilmari's published fragments, counts, pane state, socket path, and MCP URL.
Then remove the `@plugin` line and uninstall through TPM.

## Configuration

No configuration file is required. When present, Ilmari reads
`$XDG_CONFIG_HOME/ilmari/config.toml`, falling back to
`~/.config/ilmari/config.toml`. Malformed TOML, an unknown table/field, or an
invalid typed value produces a clear startup error rather than being silently
ignored.

This complete example also shows the built-in defaults:

```toml
[runtime]
refresh_seconds = 5
process_refresh_seconds = 15

[scanner]
git = true
output_tail = true

[tui]
enabled = true
bell = true

[palette]
# colors = "fg,bg,black,red,green,yellow,blue,magenta,cyan,white,bright_black,bright_red,bright_green,bright_yellow,bright_blue,bright_magenta,bright_cyan,bright_white"

[socket]
enabled = false
# path = "/home/me/.local/run/ilmari.sock"

[mcp]
enabled = false
port = 62778

[view]
app = false
git = true
detail = false
time = true
output = true
stats = false
remember = true

[badges]
enabled = true
separator = " "

[badges.running]
symbol = "●"
style = "fg=blue"

[badges.waiting_input]
symbol = "?"
style = "fg=yellow"

[badges.finished]
symbol = "✓"
style = "fg=green"

[status]
enabled = true
separator = " "

[status.running]
symbol = "R"
style = "fg=blue"

[status.waiting_input]
symbol = "I"
style = "fg=yellow"

[status.finished]
symbol = "F"
style = "fg=green"
```

`tui.enabled` defaults to `true` when the binary includes the `tui` feature and
to `false` otherwise. Omitting `[palette].colors` uses the terminal ANSI/reset palette. A socket path
implies `socket.enabled = true` unless explicitly disabled; specifying an MCP
port likewise implies `mcp.enabled = true` unless explicitly disabled. Use port
`0` for an OS-assigned free port.

Command-line flags override TOML for one run:

| Flag | Description |
| --- | --- |
| `--refresh-seconds <SECONDS>` | Main tmux scan cadence. Positive integer seconds. |
| `--process-refresh-seconds <SECONDS>` | CPU and memory sampling cadence. Positive integer seconds. |
| `--palette <CSV>` | 18-slot terminal palette override. |
| `--no-tui` | Run without the terminal UI. Use with `--socket` or `--mcp` for headless publishing. |
| `--no-git` | Start with git summaries hidden. |
| `--no-output-tail` | Disable `tmux capture-pane` output tails. |
| `--no-bell` | Disable terminal bell alerts. |
| `--socket` / `--no-socket` | Enable or disable local JSON socket publishing. |
| `--socket-path <PATH>` | Override the local JSON socket path; implies `--socket` unless `--no-socket` is also supplied. |
| `--mcp` / `--no-mcp` | Enable or disable the loopback MCP resource server. |
| `--mcp-port <PORT>` | Override the MCP loopback port; implies `--mcp` unless `--no-mcp` is also supplied. |
| `-h`, `--help` | Print help. |
| `-V`, `--version` | Print version. |

The non-secret `ILMARI_*` settings used by older releases are no longer read.
Standard discovery variables remain in use: `TMUX` and `TMUX_PANE` identify the
active tmux context, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, and `XDG_RUNTIME_DIR`
select standard storage locations, and normal home/user/runtime-directory lookup
supplies fallbacks.

### Remembered views

With `view.remember = true` (the default), toggling app, git, detail, time,
output, or stats writes only those six booleans to a versioned JSON file at
`$XDG_STATE_HOME/ilmari/view.json`, falling back to
`~/.local/state/ilmari/view.json`. The replacement is atomic and private on
Unix. Pane output text, bell state, selection, subprocess expansion, and agent
state are never saved.

Each field resolves independently: a CLI override wins when one exists, then an
explicit TOML value, then the remembered value, then the built-in default.
Configured and remembered choices remain pinned instead of being changed by
responsive layout defaults. Press uppercase `R` to clear remembered state and
restore explicit TOML choices or built-ins. A corrupt state file is left in
place, ignored, and reported as a visible warning.

### Palette format

`--palette` and `[palette].colors` accept an 18-slot CSV palette in this order:

```text
fg,bg,black,red,green,yellow,blue,magenta,cyan,white,bright_black,bright_red,bright_green,bright_yellow,bright_blue,bright_magenta,bright_cyan,bright_white
```

Accepted color formats:

- `#RRGGBB`
- `0xRRGGBB`
- `0XRRGGBB`
- `RRGGBB`
- `rgb:RR/GG/BB`
- `rgb:RRRR/GGGG/BBBB`

Malformed palette values produce a clear configuration or argument error.

## Build-time features

The default Cargo build includes the TUI and both publishing endpoints:

```bash
cargo build
```

You can remove features when building:

```bash
cargo build --no-default-features
cargo build --no-default-features --features tui
cargo build --no-default-features --features socket
cargo build --no-default-features --features mcp
cargo build --no-default-features --features rmcp
cargo build --no-default-features --features socket,mcp
cargo build --no-default-features --features tui,socket,mcp
```

| Feature | Effect |
| --- | --- |
| `tui` | Builds the terminal UI and pulls in `ratatui` and `crossterm`. |
| `socket` | Builds the local Unix JSON socket endpoint. |
| `mcp` | Builds the loopback MCP resource endpoint and pulls in `rmcp`, `axum`, `tokio`, and `tokio-util`. |
| `rmcp` | Alias for `mcp`, for builds that name the backing crate explicitly. |

Runtime flags remain parseable when an endpoint is compiled out. Enabling a
compiled-out endpoint reports a status warning such as `socket support was not
compiled in` or `MCP support was not compiled in`. Builds without feature `tui`
run in headless mode.

## Local JSON socket

Enable the socket:

```bash
ilmari --socket
```

Run it without the TUI:

```bash
ilmari --no-tui --socket
```

The socket is a local Unix-domain socket, not a network listener. By default,
Ilmari puts it under `$XDG_RUNTIME_DIR` when that variable is set, otherwise
under `/tmp/ilmari-<user>/<tmux-hash>/ilmari.sock`. When Ilmari owns the
socket, it publishes the active path to the tmux global option
`@ilmari_socket_path`:

```bash
tmux show-option -gqv @ilmari_socket_path
```

If another live Ilmari process already owns the configured socket path, the
later process keeps running without taking over that endpoint.

The socket accepts one line command per request and returns one JSON object:

```text
ping
list
ls
snapshot
detail 12
detail %12
detail tmux:%12
detail ilmari://1/7/12
read ilmari://1/7/12
```

`detail` accepts bare pane numbers, tmux pane ids, `tmux:%12` ids, and Ilmari
resource URIs. `read` accepts Ilmari resource URIs. Responses use canonical tmux
pane ids such as `%12`.

`snapshot` is an additive, render-neutral full-state response for one refresh.
Its versioned JSON includes the enabled sessions, pane/session/window identity,
agent status and detail, output excerpt, process usage, workspace and git facts,
activity timestamps, warnings, revision, observation time, and TTL. Consumers
should reject incompatible versions and stale observations. The existing `ping`,
`list`/`ls`, `detail`, and `read` request/response contracts remain available.

`list` returns a compact action queue for consumers. Each item includes the pane
id, resource URI, consumer state, agent slug, workspace path, and a suggested
next action:

```json
{
  "ok": true,
  "type": "list",
  "schema_version": 1,
  "resource": "ilmari://list",
  "items": [
    {
      "resource": "ilmari://1/7/12",
      "id": "%12",
      "state": "needs-input",
      "agent": "codex",
      "cwd": "/workspace/ilmari",
      "next": {
        "intent": "inspect",
        "kind": "tmux",
        "argv": ["tmux", "-S", "/path/to/tmux.sock", "capture-pane", "-p", "-J", "-t", "%12", "-S", "-80"]
      }
    }
  ],
  "warnings": []
}
```

State mapping:

| Ilmari status | Consumer state | Next intent |
| --- | --- | --- |
| `running` | `running` | `wait` |
| `waiting-input` | `needs-input` | `inspect` |
| `finished` | `done` | `result` |
| `terminated` | `gone` | `cleanup` |
| `unknown` | `unknown` | `inspect` |

## MCP resources

Enable the MCP server:

```bash
ilmari --no-tui --mcp
```

By default, Ilmari starts a local-only Streamable HTTP MCP server at
`http://127.0.0.1:62778/mcp`. Use `[mcp].port` or `--mcp-port <PORT>` to select
another port, or use port `0` for an OS-assigned free port.

When the server starts, Ilmari publishes the active URL to the tmux global
option `@ilmari_mcp_url`:

```bash
tmux show-option -gqv @ilmari_mcp_url
```

The MCP server exposes resources only. It does not expose tools.

| Resource | Shape |
| --- | --- |
| `ilmari://list` | Same JSON as socket `list`. |
| `ilmari://<session>/<window>/<pane>` | Same JSON as socket `detail`, using sigil-free tmux ids such as `ilmari://1/7/12`. |

Resource descriptors are plain for client compatibility. Resource contents carry
read-only metadata. Resource subscriptions are supported:

- Subscribers to a pane URI receive `notifications/resources/updated` when that
  pane resource changes.
- Subscribers to `ilmari://list` receive `notifications/resources/updated` when
  the list JSON changes.
- Subscribers to `ilmari://list` receive `notifications/resources/list_changed`
  when the visible resource set changes.

## Pane output privacy

By default, Ilmari captures recent output from supported agent panes with
`tmux capture-pane` so it can classify waiting states and show output excerpts.
Pane buffer text can contain prompts, command output, file paths, tokens, or
other sensitive information.

Disable output-tail capture when you do not want Ilmari to read pane contents:

Set `scanner.output_tail = false` in `config.toml`, or disable capture for one
run:

```bash
ilmari --no-output-tail
```

Disabling output tails can reduce classification quality for adapters that need
recent terminal text.

The JSON socket is local-only and the default managed socket directories are
created as private user-owned directories. The MCP server binds to loopback only.
Both endpoints still expose pane state to local clients that can reach the
configured socket path or loopback port.

## Troubleshooting

### `no supported agent sessions detected`

Cause: tmux has no visible panes matching enabled agent adapters, or output-tail
capture is disabled for an adapter that needs recent terminal text.

Fix:

1. Start one of the supported agent CLIs in a tmux pane.
2. Run `ilmari` from inside tmux.
3. Re-enable output-tail capture if you disabled it.

### The popup does not open

Cause: tmux popup support needs tmux 3.2 or newer, or the binding is not loaded.

Fix:

1. Check the tmux version:

   ```bash
   tmux -V
   ```

2. Reload your tmux config:

   ```bash
   tmux source-file ~/.tmux.conf
   ```

3. Confirm `ilmari` is on the `PATH` visible to tmux.

### Headless mode appears to do nothing

Cause: `--no-tui` disables the terminal UI. Without `--socket` or `--mcp`, there
is no consumer-facing endpoint.

Fix:

```bash
ilmari --no-tui --socket
ilmari --no-tui --mcp
```

### `socket support was not compiled in`

Cause: the binary was built without the `socket` feature.

Fix:

```bash
cargo build --features socket
```

### `MCP support was not compiled in`

Cause: the binary was built without the `mcp` feature.

Fix:

```bash
cargo build --features mcp
```

### `refusing to use socket directory`

Cause: the configured socket parent directory is not private, is not owned by
the current user, or is not a directory.

Fix: choose a private socket path or update the directory ownership and
permissions before starting Ilmari.

## Development

Run the same checks as CI:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

For endpoint feature coverage, also run:

```bash
cargo test --all-features
```

Source map:

| Area | Source |
| --- | --- |
| CLI flags and help text | [`src/cli.rs`](src/cli.rs) |
| Runtime config, refresh loop, key handling | [`src/app.rs`](src/app.rs) |
| Agent support and classification | [`src/agents/mod.rs`](src/agents/mod.rs), [`src/agents/adapters`](src/agents/adapters) |
| Shared status and view model types | [`src/model.rs`](src/model.rs) |
| tmux snapshot, output capture, and pane jumps | [`src/tmux.rs`](src/tmux.rs) |
| Unix JSON socket and published JSON shapes | [`src/ipc.rs`](src/ipc.rs) |
| MCP resource server | [`src/mcp.rs`](src/mcp.rs) |
| Terminal rendering and footer controls | [`src/ui.rs`](src/ui.rs) |
| Palette parsing | [`src/colors.rs`](src/colors.rs) |

## License

MIT. See [`LICENSE`](LICENSE).
