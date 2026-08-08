# Changelog

## Unreleased

## 0.11.0 - 2026-08-08

### Added

- Added Meta Muse CLI session detection for opaque-suffixed `muse-*` processes, temporary working blocks, `muse-spark` model/effort footers, and sanitized reply excerpts.

## 0.10.0 - 2026-08-04

### Added

- Added Cursor CLI (`cursor` / `cursor-agent`) session detection from live tmux process and output samples, including runtime-wrapped identity recovery, composing and tool-running states, approval prompts, model details, and sanitized output excerpts. (#11)

## 0.9.7 - 2026-07-25

### Fixed

- Automatically restart failed tmux daemon launches and unexpected daemon exits with generation-bound, capped exponential backoff while avoiding duplicate supervisors on plugin reload.

## 0.9.6 - 2026-07-21

### Fixed

- Recognize Copilot CLI's compact animated `○ Working` footer, including narrow-pane bouncing-dash variants, as running without requiring the optional `esc cancel` hint.

## 0.9.5 - 2026-07-20

### Fixed

- Detect Amp's current built-in modes and free-form custom mode labels from its light, heavy, or double prompt borders, including borders joined to preceding full-width rows by `capture-pane -J`, while retaining legacy mode support.
- Ignore process-tree agent hints while Ilmari, Yazi, Lazygit, or another unrelated application owns the pane foreground.

## 0.9.4 - 2026-07-20

### Fixed

- Track Amp's animated `≈`/`≋`/`∼` activity footer, status text, and token-count changes as running activity instead of discarding the entire bottom border as terminal chrome.

## 0.9.3 - 2026-07-20

### Fixed

- Added end-to-end coverage for automatic Ilmari daemon recovery when tmux restarts at the same socket path, ensuring the stale generation is cooperatively replaced by the current server's daemon.

## 0.9.2 - 2026-07-17

### Fixed

- Fixed tmux socket-generation verification on GNU/Linux by using GNU `stat -c` before the BSD `stat -f` fallback, so TPM startup and lifecycle guards bind the originating server correctly.

## 0.9.1 - 2026-07-17

### Added

- Added prebuilt `cargo binstall ilmari` downloads for macOS and Linux, including GNU-host mapping to Ilmari's static musl release assets.
- Added the `npx @bnomei/ilmari` launcher, which downloads, verifies, and caches the matching macOS or Linux release binary.

## 0.9.0 - 2026-07-17

### Added

- Added a default-visible popup attention column (the shared attention icon for the explicit unacknowledged latch, blank otherwise), with `n`, `[view].attention`, and remembered-state support.
- Added typed shared `[states]` icon/color presentations for popup rows, tmux window badges, and global summaries, including palette-resolved, ANSI, RGB, and default color forms plus a separate sticky-attention glyph.
- Added explicit global tmux counts for combined attention and ordinary waiting, finished, terminated, and unknown states while retaining the existing notification count options.
- Added durable pane-local `@ilmari_attention` publication so sticky attention survives a popup's direct-scan fallback when a daemon snapshot is unavailable.
- Added TPM integration coverage and a CI feature matrix for no-default, TUI-only, socket-only, MCP-only, and full builds.

### Changed

- Daemon startup now cooperatively replaces an owned healthy daemon when its configured socket path changes, without accepting a foreign endpoint.
- Responsive optional popup columns collapse only below 100 cells unless the user has explicitly pinned their visibility.

### Fixed

- Kept selected-window backgrounds continuous across multiple tmux agent badges and aligned the default global summary glyphs with badge glyphs.
- Made tmux window badges follow the effective popup app-column setting, rendering state symbols without agent names whenever that column is hidden.
- Validated all tmux-rendered badge and status symbols and separators, and restricted shared `[states]` icons to one safe terminal cell, rejecting format delimiters and display-width mismatches.
- Kept popup selection neutral and full-width while preserving configured attention and lifecycle glyph foreground colors.
- Cleared all daemon-owned pane and global renderer options during TPM fallback cleanup, and hardened tmux format escaping and numeric window ordering.

## 0.8.0 - 2026-07-16

### Added

- Added a provider-neutral, singleton daemon per tmux server, with foreground `daemon start`, `daemon stop`, and `daemon status` commands plus a compact `ilmari status` helper.
- Added a versioned full-state JSON socket snapshot so popups can reuse one fresh daemon collection without per-pane request fan-out while preserving the existing socket commands and MCP resources.
- Added exact-pane sticky attention tracking and tmux-published per-window badges and global running, waiting-input, and unacknowledged-finished counts for every enabled agent adapter.
- Added optional, strongly typed XDG TOML configuration for runtime, scanner, TUI, palette, socket, MCP, view, badge, and status settings, with built-in defaults and strict unknown-field validation.
- Added versioned XDG view-state persistence for the six popup views, including immediate save on toggle and `R` to clear remembered choices.
- Added a TPM tmux plugin entrypoint that keeps the configurable popup binding, starts the per-server daemon by default, supports explicit daemon opt-out and command overrides, and leaves user layout and theme formats under user control.
- Added the paired TPM `@ilmari_daemon_stop_command` option so customized daemon start commands, including wrappers and arguments, have an explicit reliable stop path.

### Changed

- Popup refreshes now prefer a compatible daemon snapshot within its TTL, retry direct tmux scanning when daemon data is absent, stale, malformed, or incompatible, and retain the last good rows with a warning if both sources fail.
- All tmux subprocesses are pinned to the originating tmux socket so separate tmux servers do not share daemon, scan, focus, or published state accidentally.
- Window badges and status summaries are explicit user-placeable tmux format fragments; Ilmari never rewrites `window-status-format`, `window-status-current-format`, `status-left`, or `status-right`.
- Removed non-secret `ILMARI_*` configuration environment variables in favor of TOML and existing one-run CLI overrides. Standard runtime discovery variables such as `TMUX`, `TMUX_PANE`, and `XDG_*` remain supported.
- Raised the minimum supported Rust version to 1.88.0 to match the resolved runtime and TUI dependency graph.

### Fixed

- Focus acknowledgement now uses exact focused-pane facts across tmux clients, clears attention only for the pane actually viewed, avoids recreating attention on unchanged scans, and removes published state for panes that disappear.
- Daemon shutdown, signals, TPM opt-out, and vanished tmux servers now clean stale pane/global options instead of leaving badges, counts, or discovery paths behind.
- TPM lifecycle routing now preserves tmux socket paths containing commas by parsing the numeric server/client suffix fields from the right.

## 0.7.0 - 2026-06-29

### Changed

- Improve agent-pane detection for wrapped or remote sessions, including SSH-hosted Gemini, Auggie, and Antigravity panes where the tmux title is the reliable signal.
- Preserve bell alert baselines through warning-only tmux snapshot parses so malformed `list-panes` rows do not suppress later status-transition alerts.
- Keep real Claude Code answers that start with `Tip:` in output excerpts while still filtering known Claude footer chrome.

### Fixed

- Validate pre-existing explicit `ILMARI_SOCKET_PATH` parent directories outside the runtime base before binding sockets, rejecting insecure shared-directory setups.

## 0.6.2 - 2026-06-25

### Fixed

- Keep Claude Code bottom chrome out of output excerpts, including auto-mode, agents-footer, activity-token, tip, and quota-status rows, so Ilmari shows the latest meaningful agent output instead of transient status text.
- Recognize Claude Code auto-mode prompt footers as waiting-input prompts.
- Replace personal absolute paths in README socket examples with neutral workspace paths.

## 0.6.1 - 2026-06-22

### Changed

- Changed the default MCP loopback port to `62778` while keeping `--mcp-port 0` as the explicit ephemeral-port option.
- Kept MCP resource descriptors plain for client compatibility while preserving read-only metadata on resource contents.
- Removed the now-unused direct `chrono` dependency from the MCP feature.

### Fixed

- Fixed Codex MCP resource propagation by avoiding optional `resources/list` descriptor fields that can trigger `Unexpected response type`.

## 0.6.0 - 2026-06-21

### Added

- Added opt-in local JSON socket publishing with `ping`, `list`, `ls`, `detail`, and `read` commands for connector processes.
- Added opt-in loopback MCP resource publishing with read-only `ilmari://list` and pane detail resources, resource subscriptions, read-only metadata, and assistant-focused resource annotations.
- Added `--no-tui` / `ILMARI_TUI=0` headless mode for running Ilmari as a state publisher without the terminal UI.
- Added build-time `tui`, `socket`, `mcp`, and `rmcp` feature controls so endpoint and UI support can be compiled independently.

### Changed

- Made socket and MCP resources share the same list/detail JSON shapes, including compact consumer states, resource URIs, and prebuilt tmux command argv values.
- Changed MCP `resources/list_changed` behavior to report only resource-set changes while list content updates continue through `notifications/resources/updated`.

### Fixed

- Stop MCP subscription tasks when notification delivery fails so disconnected subscribers do not leave live update loops behind.

## 0.5.0 - 2026-06-21

### Added

- Added GitHub Copilot CLI (`copilot`) and Kiro CLI (`kiro-cli`) session detection, including tmux output signatures, runtime-wrapper process matching, model details, status classification, and sanitized output excerpts.

### Changed

- Shortened app column labels for GitHub Copilot and Kiro to `Copilot` and `Kiro`.
- Compacted the README agent support section into inline supported and planned CLI lists, with planned CLI names linking directly to their tracking issues.

### Fixed

- Keep completed Kiro replies visible when older active `Thinking...` lines remain in the captured tail.
- Preserve Antigravity active detection for cropped `esc to cancel` tails without cross-matching Kiro activity.

## 0.4.0 - 2026-06-19

### Added

- Added Antigravity CLI (`agy`) session detection, including active generation spinners, permission prompts, model footers, and sanitized output excerpts.
- Listed Antigravity CLI as a supported agent while keeping Gemini CLI support documented for users who still have access.

## 0.3.0 - 2026-06-17

### Added

- Added runtime CLI, config, and environment wiring for pane filtering, output tails, subprocess visibility, stats, color, ticker, refresh timing, and version/help behavior. (#6)
- Added pane output tail opt-out support and precedence coverage for CLI flags, environment variables, and config files. (#8)

### Changed

- Split built-in agent adapters into dedicated modules behind the shared adapter registry. (#5)

### Fixed

- Propagate pane output tail capture failures into the TUI model so compact rows can surface capture errors instead of silently dropping pane output. (#7)

## 0.2.5 - 2026-06-08

### Changed

- Continued the June 8 release push with updated agent CLI detection fixtures and release-readiness validation.
- Prefer Grok reply text over `Turn completed in ...` status lines when an idle turn still has visible response content.

### Fixed

- Detect updated Codex status bars when model labels starting with `gpt-` are surrounded by optional usage, context, or quota fields.
- Detect updated Amp, Auggie, Gemini, and Grok model/mode displays, including Amp `deep²`, `smart`, and `↯` mode modifiers, Gemini table footers, and Grok `Composer 2.5` footers.
- Render Amp mode colors to match Amp itself: `deep²` bluish, `smart` green, and `rush` yellow.
- Keep Amp compact prompt chrome, Gemini footer tables, and Grok completion chrome out of output excerpts when they are not the latest meaningful response.
- Replace local setup paths and personal fixture labels with neutral workspace fixtures.

## 0.2.4 - 2026-06-08

### Fixed

- Refresh process usage when stats or subprocess expansion is enabled, clear stale cached usage when stats are hidden or `ps` fails, and keep expanded subprocess stats aligned when model details are hidden.

## 0.2.3 - 2026-06-08

### Fixed

- Detect node-wrapped Auggie sessions via cached process identity, including `node .../bin/auggie`, without relying on pane title or output text.
- Tighten process-based agent identity matching to executable and known wrapper paths so ordinary commands mentioning agent names do not create agent rows.

## 0.2.2 - 2026-06-05

### Fixed

- Treat Grok active `Waiting...` footers as running when Grok is waiting on subagent work, so timer and spinner updates do not make Ilmari report the parent session as idle.

## 0.2.1 - 2026-06-25

### Added

- `Esc` now quits the popup alongside `q` (and Ctrl-C) for better muscle memory with other tmux popup tools. (Closes #2, thanks @phinze)

### Fixed

- Fixed Claude Code pane detection for wrapped binaries and rewritten titles (`✳`, or confirmed Braille spinners), without process-tree crawling. (Fixes #1, thanks @phinze)

## 0.2.0 - 2026-06-05

### Added

- Added Grok session detection across tmux command, title, and output signals.
- Added Grok process usage attribution.
- Added Grok output parsing for standard, compact, and command-palette UI states.

### Changed

- Show Grok `Turn completed in ...` status lines as the latest output excerpt when a turn is idle.
- Scoped Grok-specific prompt classification to Grok so other agents are not affected by Grok UI chrome.
- Tightened generic approval prompt detection so Grok `always-approve` mode labels are not treated as waiting prompts.
