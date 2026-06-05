# Changelog

## Unreleased

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
