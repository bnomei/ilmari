# Changelog

## Unreleased

### Added

- `Esc` now quits the popup alongside `q` (and Ctrl-C) for better muscle memory with other tmux popup tools.

### Fixed

- Claude Code panes are now detected reliably for wrapped binaries (e.g. Nix `.claude-unwrapped`) via improved command normalization, and for rewritten task-summary titles via branded glyph prefixes (`✳`, Braille spinners like `⠐` etc.). No process tree crawling is used for detection. (Addresses https://github.com/bnomei/ilmari/issues/1)

## 0.2.0 - 2026-06-05

### Added

- Added Grok session detection across tmux command, title, and output signals.
- Added Grok process usage attribution.
- Added Grok output parsing for standard, compact, and command-palette UI states.

### Changed

- Show Grok `Turn completed in ...` status lines as the latest output excerpt when a turn is idle.
- Scoped Grok-specific prompt classification to Grok so other agents are not affected by Grok UI chrome.
- Tightened generic approval prompt detection so Grok `always-approve` mode labels are not treated as waiting prompts.
