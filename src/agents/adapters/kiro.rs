//! Kiro CLI adapter with title and output-tail identity fallbacks.
//!
//! Command match is `kiro-cli`; wrapped launches are recovered via title and output heuristics.
//! Repository: https://github.com/kirodotdev/Kiro
//! Docs: https://kiro.dev/docs/cli/

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{
    classify_kiro_session, command_matches, extract_kiro_detail, extract_kiro_output_excerpt,
    is_shell_command, looks_like_kiro_output, pane_title_contains, reuse_detail_arc,
    reuse_output_excerpt_arc, AgentAdapter,
};

/// Enabled Kiro CLI adapter (`kiro-cli`) with title/output identity fallbacks.
pub(in crate::agents) struct KiroCliAdapter;

impl AgentAdapter for KiroCliAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::KiroCli
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "kiro-cli")
            || (!is_shell_command(&pane.pane_current_command)
                && pane_title_contains(&pane.pane_title, "kiro"))
    }

    fn detect_output(&self, _pane: &PaneSnapshot, output_tail: &str) -> bool {
        looks_like_kiro_output(output_tail)
    }

    fn classify(
        &self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        output_fingerprint: Option<u64>,
        previous: Option<&SessionRecord>,
    ) -> SessionStatus {
        classify_kiro_session(self, pane, output_tail, output_fingerprint, previous)
    }

    fn extract_detail(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<AgentDetail>> {
        reuse_detail_arc(extract_kiro_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_kiro_output_excerpt(output_tail), previous)
    }
}
