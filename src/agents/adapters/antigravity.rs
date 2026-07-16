//! Antigravity CLI adapter with custom session classifier and output heuristics.
//! Repository: https://github.com/google-gemini/antigravity-cli

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{
    classify_antigravity_session, command_matches, extract_antigravity_detail,
    extract_antigravity_output_excerpt, is_shell_command, looks_like_antigravity_output,
    pane_title_contains, reuse_detail_arc, reuse_output_excerpt_arc, AgentAdapter,
};

/// Enabled Antigravity CLI adapter (`agy`) with custom status ladder.
pub(in crate::agents) struct AntigravityAdapter;

impl AgentAdapter for AntigravityAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::AntigravityCli
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "agy")
            || (!is_shell_command(&pane.pane_current_command)
                && pane_title_contains(&pane.pane_title, "antigravity"))
    }

    fn detect_output(&self, _pane: &PaneSnapshot, output_tail: &str) -> bool {
        looks_like_antigravity_output(output_tail)
    }

    fn classify(
        &self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        output_fingerprint: Option<u64>,
        previous: Option<&SessionRecord>,
    ) -> SessionStatus {
        classify_antigravity_session(self, pane, output_tail, output_fingerprint, previous)
    }

    fn extract_detail(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<AgentDetail>> {
        reuse_detail_arc(extract_antigravity_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_antigravity_output_excerpt(output_tail), previous)
    }
}
