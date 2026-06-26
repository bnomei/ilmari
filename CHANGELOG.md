# Changelog

## Unreleased

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
