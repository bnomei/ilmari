# ilmari

[![Crates.io Version](https://img.shields.io/crates/v/ilmari)](https://crates.io/crates/ilmari)
[![CI](https://img.shields.io/github/actions/workflow/status/bnomei/ilmari/ci.yml?branch=main)](https://github.com/bnomei/ilmari/actions/workflows/ci.yml)
[![Crates.io Downloads](https://img.shields.io/crates/d/ilmari)](https://crates.io/crates/ilmari)
[![License](https://img.shields.io/crates/l/ilmari)](https://crates.io/crates/ilmari)
[![Discord](https://flat.badgen.net/badge/discord/bnomei?color=7289da&icon=discord&label)](https://discordapp.com/users/bnomei)
[![Buymecoffee](https://flat.badgen.net/badge/icon/donate?icon=buymeacoffee&color=FF813F&label)](https://www.buymeacoffee.com/bnomei)

Minimal tmux popup radar that can be used standalone like a sidebar or as a tmux-popup.

Supported agent CLIs: Antigravity CLI (`agy`), Gemini CLI (`gemini`), Codex (`codex`), Amp (`amp`), Claude Code (`claude`), OpenCode (`opencode`), Pi (`pi`, `pi-agent`), Auggie (`auggie`), Grok (`grok`), GitHub Copilot CLI (`copilot`), and Kiro CLI (`kiro-cli`).

Planned agent CLI support: [Cursor CLI](https://github.com/bnomei/ilmari/issues/11) (`cursor`), [Aider](https://github.com/bnomei/ilmari/issues/12) (`aider`), [Cline CLI](https://github.com/bnomei/ilmari/issues/13) (`cline`), [Goose CLI](https://github.com/bnomei/ilmari/issues/14) (`goose`), and [OpenHands CLI](https://github.com/bnomei/ilmari/issues/16) (`openhands`). PRs welcome: each linked CLI name opens its tracking issue.

## Platform support

Ilmari is built for Unix-like tmux environments. The supported release
artifacts are Linux and macOS binaries, and runtime usage assumes a Unix-like
shell with tmux available. Windows is currently out of scope: no Windows release
artifact is published unless Windows tmux behavior is implemented and tested.

Ilmari exists for the moment when a tmux workspace has multiple agent panes and
you need to answer three questions quickly:

- which pane is still running
- which pane is waiting on you
- which workspace that pane belongs to

It opens as a tmux popup, inspects the panes you already have running, groups
them by workspace, shows agent state plus a visible output excerpt, and jumps
you straight to the selected pane.

Ilmari is popup-first and observer-only. It does not launch agents, manage
workflows, or own your tmux layout. It gives you a fast index over the agent
sessions that already exist.


<a title="click to open" target="_blank" style="cursor: zoom-in;" href="https://raw.githubusercontent.com/bnomei/ilmari/main/screenshot.png
"><img src="https://raw.githubusercontent.com/bnomei/ilmari/main/screenshot.png" alt="screenshot" style="width: 100%;" /></a>

## What Ilmari Solves

When several agents are running at once, the normal tmux workflow gets noisy:

- waiting panes are easy to miss
- finished panes and still-running panes blur together
- workspaces get split across sessions and windows
- you end up cycling panes manually just to find the one that matters

Ilmari reduces that to a popup:

- detect supported agent panes from tmux metadata and output
- classify panes as running, waiting, finished, terminated, or unknown
- group panes by workspace path
- show lightweight git change context per workspace
- jump to the selected pane and get out of the way

## Installation

### Cargo (crates.io)

```sh
cargo install ilmari
```

### Homebrew

```bash
brew install bnomei/ilmari/ilmari
```

### GitHub Releases

Download a prebuilt Linux or macOS archive from the GitHub Releases page,
extract it, and place `ilmari` on your `PATH`. Windows archives are not
published because Windows runtime behavior is not supported yet.

### From source

```bash
git clone https://github.com/bnomei/ilmari.git
cd ilmari
cargo build --release
```

## Popup-First Setup

tmux popup support requires tmux 3.2 or newer.

### 1. Add a tmux binding

Assuming `ilmari` is already installed and available on your `PATH`:

```tmux
bind-key i display-popup -E -w 90% -h 85% "ilmari"
```

Reload tmux after editing `~/.tmux.conf`:

```sh
tmux source-file ~/.tmux.conf
```

### 2. Open the popup

Press your tmux prefix, then the bound key.

Typical popup flow:

- open `ilmari`
- move selection with `j` / `k` or arrow keys
- press `Enter` to jump to the selected pane
- press `q` or `Esc` to quit

When `ilmari` is launched as a tmux popup, activation returns you to the target
pane and closes the popup.

## Configuration

Ilmari configures through command-line flags and environment variables. It reads
environment defaults first, then command-line flags override the same setting for
that run.

| Flag | Purpose | Env fallback |
| --- | --- | --- |
| `--refresh-seconds <SECONDS>` | Main tmux scan cadence | `ILMARI_REFRESH_SECONDS` |
| `--process-refresh-seconds <SECONDS>` | CPU and memory sampling cadence | `ILMARI_PROCESS_REFRESH_SECONDS` |
| `--palette <CSV>` | 18-slot palette override | `ILMARI_TUI_PALETTE`, then `ILMARI_PALETTE` |
| `--no-git` | Start with git summaries hidden | none |
| `--no-output-tail` | Skip pane output tail capture | `ILMARI_OUTPUT_TAIL=0` |
| `--no-bell` | Disable terminal bell alerts | none |
| `--help` / `-h` | Print help | none |
| `--version` / `-V` | Print version | none |

| Variable | Purpose | Default | Notes |
| --- | --- | --- | --- |
| `ILMARI_REFRESH_SECONDS` | Main tmux scan cadence | `5` | Positive integer seconds. Empty, invalid, or non-positive values fall back to the default. |
| `ILMARI_PROCESS_REFRESH_SECONDS` | CPU and memory sampling cadence | `15` | Separate from the main refresh so `ilmari` does not call `ps` on every redraw. Empty, invalid, or non-positive values fall back to the default. |
| `ILMARI_OUTPUT_TAIL` | Pane output tail capture | enabled | Set to `0`, `false`, `no`, or `off` to skip `tmux capture-pane` output tail capture. |
| `ILMARI_TUI_PALETTE` | Primary palette override | terminal ANSI theme | Takes an 18-slot CSV palette. Takes precedence over `ILMARI_PALETTE`. |
| `ILMARI_PALETTE` | Compatibility alias for palette override | terminal ANSI theme | Used only when `ILMARI_TUI_PALETTE` is unset. |

Examples:

```sh
ILMARI_REFRESH_SECONDS=10 ilmari
ILMARI_PROCESS_REFRESH_SECONDS=30 ilmari
ILMARI_OUTPUT_TAIL=0 ilmari
```

### Pane output privacy

By default, Ilmari captures a small recent tail from supported agent panes with
`tmux capture-pane` so it can classify waiting states and show the latest output
excerpt. That pane buffer/history text may contain prompts, command output, file
paths, tokens, or other sensitive information. If you do not want Ilmari to read
pane contents, disable output tail capture with `ILMARI_OUTPUT_TAIL=0`; pane
tails remain enabled by default unless that variable is set to `0`, `false`,
`no`, or `off`.

Ilmari uses semantic color roles in code, but by default those resolve through
your terminal's current ANSI palette. If you want explicit colors, provide an
18-slot CSV palette override using `ILMARI_TUI_PALETTE` or `ILMARI_PALETTE`.

Slot order:

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

Behavior:

- `ILMARI_TUI_PALETTE` takes precedence over `ILMARI_PALETTE`
- empty or malformed palette values are ignored
- if neither is set, `ilmari` uses the terminal's default ANSI theme
