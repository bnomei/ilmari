//! Shared domain types for agent sessions, TUI view models, and published-state serialization.
//!
//! `SessionRecord` is the per-pane runtime source of truth; `AppModel` is the denormalized
//! render snapshot rebuilt on every refresh cycle.

use crate::tmux::PaneSnapshot;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Supported coding-agent identity used for detection, display, and process matching.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    Codex,
    Amp,
    ClaudeCode,
    OpenCode,
    Pi,
    GeminiCli,
    AntigravityCli,
    Auggie,
    Grok,
    Muse,
    GitHubCopilotCli,
    CursorCli,
    Aider,
    ClineCli,
    GooseCli,
    KiroCli,
    OpenHandsCli,
}

/// Whether an `AgentKind` is active in v1 or tracked as planned work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSupport {
    Enabled,
    Planned { issue: u32 },
}

impl AgentKind {
    /// Every known agent identity, including planned (disabled) kinds.
    pub const ALL_KINDS: [Self; 17] = [
        Self::Codex,
        Self::Amp,
        Self::ClaudeCode,
        Self::OpenCode,
        Self::Pi,
        Self::GeminiCli,
        Self::AntigravityCli,
        Self::Auggie,
        Self::Grok,
        Self::Muse,
        Self::GitHubCopilotCli,
        Self::CursorCli,
        Self::Aider,
        Self::ClineCli,
        Self::GooseCli,
        Self::KiroCli,
        Self::OpenHandsCli,
    ];

    /// Enabled for v1 detection, or planned with a tracking GitHub issue number.
    pub const fn support(self) -> AgentSupport {
        match self {
            Self::Codex
            | Self::Amp
            | Self::ClaudeCode
            | Self::OpenCode
            | Self::Pi
            | Self::GeminiCli
            | Self::AntigravityCli
            | Self::Auggie
            | Self::Grok
            | Self::Muse
            | Self::GitHubCopilotCli
            | Self::CursorCli
            | Self::KiroCli => AgentSupport::Enabled,
            Self::Aider => AgentSupport::Planned { issue: 12 },
            Self::ClineCli => AgentSupport::Planned { issue: 13 },
            Self::GooseCli => AgentSupport::Planned { issue: 14 },
            Self::OpenHandsCli => AgentSupport::Planned { issue: 16 },
        }
    }

    /// Whether this kind participates in detection and classification today.
    pub const fn is_enabled(self) -> bool {
        matches!(self.support(), AgentSupport::Enabled)
    }

    /// Iterator over kinds active in the v1 adapter registry.
    pub fn enabled_kinds() -> impl Iterator<Item = Self> {
        Self::ALL_KINDS.into_iter().filter(|kind| kind.is_enabled())
    }

    /// Iterator over planned (disabled) kinds for registry and fixture tests.
    #[cfg(test)]
    pub fn planned_kinds() -> impl Iterator<Item = Self> {
        Self::ALL_KINDS.into_iter().filter(|kind| !kind.is_enabled())
    }

    /// Human-facing radar label (stable for fixtures and status fragments).
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Amp => "Amp",
            Self::ClaudeCode => "Claude Code",
            Self::OpenCode => "OpenCode",
            Self::Pi => "Pi",
            Self::GeminiCli => "Gemini CLI",
            Self::AntigravityCli => "Antigravity",
            Self::Auggie => "Auggie",
            Self::Grok => "Grok",
            Self::Muse => "Muse",
            Self::GitHubCopilotCli => "Copilot",
            Self::CursorCli => "Cursor CLI",
            Self::Aider => "Aider",
            Self::ClineCli => "Cline CLI",
            Self::GooseCli => "Goose CLI",
            Self::KiroCli => "Kiro",
            Self::OpenHandsCli => "OpenHands CLI",
        }
    }
}

/// Classified lifecycle state for one agent pane after adapter output analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionStatus {
    /// Agent process is live and producing or actively working.
    Running,
    /// Agent is idle and appears to need human input.
    WaitingInput,
    /// Work completed; the pane may still exist as a shell after exit.
    Finished,
    /// Pane disappeared from tmux or was marked dead.
    Terminated,
    /// Identity known or suspected, but status signals are inconclusive.
    Unknown,
}

impl SessionStatus {
    /// Wire and display slug for this lifecycle state.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::WaitingInput => "waiting-input",
            Self::Finished => "finished",
            Self::Terminated => "terminated",
            Self::Unknown => "unknown",
        }
    }

    /// Whether vanished panes in this status remain visible for the retention window.
    pub fn uses_retention(self) -> bool {
        matches!(self, Self::Finished)
    }
}

/// One tracked agent session keyed by tmux pane id across refresh cycles.
///
/// `SessionTracker` owns creation and updates. Fields other than `pane` are
/// classifier or hydration outputs and may lag one refresh when capture fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// Latest tmux metadata for this pane id (path, command, title, liveness).
    pub pane: PaneSnapshot,
    /// Adapter that currently owns detection and classification for this pane.
    pub kind: AgentKind,
    /// Lifecycle state from the last successful classification (or capture hold).
    pub status: SessionStatus,
    /// Sticky, unacknowledged attention projected by the shared tmux-state latch.
    /// This is intentionally independent of the lifecycle state.
    pub attention: bool,
    /// Optional model/mode label for the radar detail column.
    pub detail: Option<Arc<AgentDetail>>,
    /// Recent non-noise output snippet shown in the radar output column.
    pub output_excerpt: Option<Arc<str>>,
    /// Optional CPU/memory rollup from process-tree hydration (stats column).
    pub process_usage: Option<Arc<SessionProcessUsage>>,
    /// Hash of the status-signal window used to detect output motion between refreshes.
    pub output_fingerprint: Option<u64>,
    /// When `kind` or `status` last changed; drives inactive-since labels and sort order.
    pub last_changed_at: Instant,
    /// Last refresh that still observed this pane id in `list-panes`.
    pub last_seen_at: Instant,
    /// Deadline after which a finished pane drops from the radar if still missing.
    pub retained_until: Option<Instant>,
}

/// CPU and resident memory sample for one process, in tenths-of-percent and KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceUsage {
    pub cpu_tenths_percent: u32,
    pub memory_kib: u64,
}

impl ResourceUsage {
    /// Zero sample used when a process tree is empty or usage is released.
    pub const fn zero() -> Self {
        Self { cpu_tenths_percent: 0, memory_kib: 0 }
    }

    /// Component-wise saturating sum for rolling agent and spawned totals.
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            cpu_tenths_percent: self.cpu_tenths_percent.saturating_add(other.cpu_tenths_percent),
            memory_kib: self.memory_kib.saturating_add(other.memory_kib),
        }
    }
}

/// One descendant process under an agent binary, shown when the stats subtask list is expanded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtaskProcess {
    pub pid: u32,
    /// Depth under the matched agent binary (1 = direct child).
    pub depth: usize,
    /// Short command label suitable for the indented stats subtask rows.
    pub command_label: String,
    pub usage: ResourceUsage,
}

/// Rolled-up process usage for the agent binary and its descendant subtasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProcessUsage {
    /// Usage of the matched agent executable itself.
    pub agent: ResourceUsage,
    /// Sum of descendant subtask usage (excludes the agent binary).
    pub spawned: ResourceUsage,
    /// Ordered descendant list for optional expansion under the stats column.
    pub subtasks: Vec<SubtaskProcess>,
}

/// Palette role for an adapter-extracted model or mode label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentDetailTone {
    /// Default yellow detail color for ordinary model names.
    Neutral,
    /// Grok model accent.
    Grok,
    /// Luna model accent.
    Luna,
    /// Terra model accent.
    Terra,
    /// Sol model accent.
    Sol,
    /// Amp deep mode accent.
    AmpDeep,
    /// Amp smart mode accent.
    AmpSmart,
    /// Amp rush mode accent.
    AmpRush,
}

/// Optional model/mode label extracted from pane output for the detail column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDetail {
    pub label: String,
    pub tone: AgentDetailTone,
}

/// Workspace bucket grouping pane rows that share a derived path label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceGroup {
    /// Disambiguated path label shared by the panes in this group.
    pub label: String,
    /// Optional git shortstat for the shared repository root.
    pub git_summary: Option<GitSummaryRow>,
    pub rows: Vec<PaneRow>,
}

/// One render-ready pane row with selection, jump-match, and visibility flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRow {
    /// Tmux pane id (e.g. `%3`); also the jump target token.
    pub pane_id: String,
    /// Wall-clock inactive-since text for non-running rows; empty while running.
    pub inactive_since_label: String,
    pub output_excerpt: Option<Arc<str>>,
    /// Agent display name for the app column.
    pub client_label: &'static str,
    pub detail: Option<Arc<AgentDetail>>,
    pub process_usage: Option<Arc<SessionProcessUsage>>,
    /// Whether the stats subtask list is expanded under this row.
    pub subtasks_expanded: bool,
    pub status: SessionStatus,
    /// Explicit sticky attention fact rendered in its own popup column.
    pub attention: bool,
    /// Stable status slug mirrored from `SessionStatus::as_str`.
    pub status_label: &'static str,
    /// Highlighted by the numeric pane-jump filter.
    pub is_jump_match: bool,
    pub is_selected: bool,
}

/// Cached git branch and unstaged diff stats for one repository root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSummaryRow {
    /// Absolute repository root path used as the cache key.
    pub workspace_path: PathBuf,
    /// Radar-facing label, often shared with the workspace group header.
    pub workspace_label: String,
    pub branch_name: String,
    pub insertions: u32,
    pub deletions: u32,
}

/// Denormalized TUI and IPC-facing snapshot built from current `SessionRecord` rows.
///
/// Rebuilt on every refresh; column flags mirror the interactive view toggles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppModel {
    pub title: String,
    /// Footer/status text for warnings and optional git summary snippets.
    pub status_line: String,
    pub show_app: bool,
    pub show_attention: bool,
    pub show_git: bool,
    pub show_detail: bool,
    pub show_time: bool,
    pub show_output: bool,
    pub show_stats: bool,
    pub workspace_groups: Vec<WorkspaceGroup>,
    pub refresh_interval: Duration,
    pub last_refresh: Instant,
    /// Wall clock of the last refresh for inactive-since labels.
    pub last_refresh_wallclock: SystemTime,
}

impl AppModel {
    /// Empty radar model shown before the first successful refresh completes.
    pub fn placeholder() -> Self {
        Self {
            title: "Agents".to_string(),
            status_line: "Waiting for tmux agent sessions.".to_string(),
            show_app: false,
            show_attention: true,
            show_git: true,
            show_detail: false,
            show_time: true,
            show_output: true,
            show_stats: false,
            workspace_groups: Vec::new(),
            refresh_interval: Duration::from_secs(5),
            last_refresh: Instant::now(),
            last_refresh_wallclock: SystemTime::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentKind, AgentSupport, AppModel, SessionStatus};
    use std::time::Duration;

    #[test]
    fn placeholder_uses_expected_defaults() {
        let model = AppModel::placeholder();

        assert_eq!(model.title, "Agents");
        assert!(!model.show_app);
        assert!(model.show_git);
        assert!(!model.show_detail);
        assert!(model.show_time);
        assert!(model.show_output);
        assert!(!model.show_stats);
        assert!(model.workspace_groups.is_empty());
        assert_eq!(model.refresh_interval, Duration::from_secs(5));
    }

    #[test]
    fn agent_display_names_are_stable() {
        assert_eq!(AgentKind::Codex.display_name(), "Codex");
        assert_eq!(AgentKind::Amp.display_name(), "Amp");
        assert_eq!(AgentKind::ClaudeCode.display_name(), "Claude Code");
        assert_eq!(AgentKind::OpenCode.display_name(), "OpenCode");
        assert_eq!(AgentKind::Pi.display_name(), "Pi");
        assert_eq!(AgentKind::GeminiCli.display_name(), "Gemini CLI");
        assert_eq!(AgentKind::AntigravityCli.display_name(), "Antigravity");
        assert_eq!(AgentKind::Auggie.display_name(), "Auggie");
        assert_eq!(AgentKind::Grok.display_name(), "Grok");
        assert_eq!(AgentKind::Muse.display_name(), "Muse");
        assert_eq!(AgentKind::GitHubCopilotCli.display_name(), "Copilot");
        assert_eq!(AgentKind::CursorCli.display_name(), "Cursor CLI");
        assert_eq!(AgentKind::Aider.display_name(), "Aider");
        assert_eq!(AgentKind::ClineCli.display_name(), "Cline CLI");
        assert_eq!(AgentKind::GooseCli.display_name(), "Goose CLI");
        assert_eq!(AgentKind::KiroCli.display_name(), "Kiro");
        assert_eq!(AgentKind::OpenHandsCli.display_name(), "OpenHands CLI");
        assert_eq!(
            AgentKind::enabled_kinds().collect::<Vec<_>>(),
            vec![
                AgentKind::Codex,
                AgentKind::Amp,
                AgentKind::ClaudeCode,
                AgentKind::OpenCode,
                AgentKind::Pi,
                AgentKind::GeminiCli,
                AgentKind::AntigravityCli,
                AgentKind::Auggie,
                AgentKind::Grok,
                AgentKind::Muse,
                AgentKind::GitHubCopilotCli,
                AgentKind::CursorCli,
                AgentKind::KiroCli,
            ]
        );
    }

    #[test]
    fn planned_agent_kinds_are_issue_tracked_but_disabled() {
        let planned = [
            (AgentKind::Aider, 12),
            (AgentKind::ClineCli, 13),
            (AgentKind::GooseCli, 14),
            (AgentKind::OpenHandsCli, 16),
        ];

        assert_eq!(
            AgentKind::planned_kinds().collect::<Vec<_>>(),
            planned.map(|(kind, _issue)| kind).to_vec()
        );
        for (kind, issue) in planned {
            assert_eq!(kind.support(), AgentSupport::Planned { issue });
            assert!(!kind.is_enabled());
        }

        for kind in AgentKind::enabled_kinds() {
            assert_eq!(kind.support(), AgentSupport::Enabled);
            assert!(kind.is_enabled());
        }
    }

    #[test]
    fn all_agent_kinds_have_support_and_display_metadata() {
        assert_eq!(AgentKind::ALL_KINDS.len(), 17);
        for kind in AgentKind::ALL_KINDS {
            assert!(!kind.display_name().is_empty());
            match kind.support() {
                AgentSupport::Enabled => assert!(kind.is_enabled()),
                AgentSupport::Planned { issue } => {
                    assert!((10..=16).contains(&issue));
                    assert!(!kind.is_enabled());
                }
            }
        }
    }

    #[test]
    fn retained_statuses_match_the_v1_contract() {
        assert!(SessionStatus::Finished.uses_retention());
        assert!(!SessionStatus::Terminated.uses_retention());
        assert!(!SessionStatus::Running.uses_retention());
        assert!(!SessionStatus::WaitingInput.uses_retention());
        assert!(!SessionStatus::Unknown.uses_retention());
    }
}
