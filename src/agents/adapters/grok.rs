//! Grok CLI adapter with custom session classifier and output heuristics.
//! Repository: https://github.com/xai-org/grok-cli

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{
    classify_grok_session, command_matches, extract_grok_detail, extract_grok_output_excerpt,
    is_shell_command, looks_like_grok_output, pane_title_contains, reuse_detail_arc,
    reuse_output_excerpt_arc, AgentAdapter,
};

/// Enabled Grok CLI adapter (`grok`) with custom status ladder.
pub(in crate::agents) struct GrokAdapter;

impl AgentAdapter for GrokAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Grok
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "grok")
            || (!is_shell_command(&pane.pane_current_command)
                && pane_title_contains(&pane.pane_title, "grok"))
    }

    fn detect_output(&self, _pane: &PaneSnapshot, output_tail: &str) -> bool {
        looks_like_grok_output(output_tail)
    }

    fn classify(
        &self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        output_fingerprint: Option<u64>,
        previous: Option<&SessionRecord>,
    ) -> SessionStatus {
        classify_grok_session(self, pane, output_tail, output_fingerprint, previous)
    }

    fn extract_detail(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<AgentDetail>> {
        reuse_detail_arc(extract_grok_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_grok_output_excerpt(output_tail), previous)
    }
}
