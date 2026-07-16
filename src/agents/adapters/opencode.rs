//! OpenCode adapter for `opencode` panes and model detail extraction.
//! Repository: https://github.com/sst/opencode
//! Docs: https://opencode.ai/docs

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{
    classify_supported_session, command_matches, extract_opencode_detail,
    extract_opencode_output_excerpt, pane_title_contains, reuse_detail_arc,
    reuse_output_excerpt_arc, AgentAdapter,
};

/// Enabled OpenCode adapter (`opencode`).
pub(in crate::agents) struct OpenCodeAdapter;

impl AgentAdapter for OpenCodeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::OpenCode
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "opencode")
            || pane_title_contains(&pane.pane_title, "oc |")
            || pane_title_contains(&pane.pane_title, "opencode")
    }

    fn classify(
        &self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        output_fingerprint: Option<u64>,
        previous: Option<&SessionRecord>,
    ) -> SessionStatus {
        classify_supported_session(self, pane, output_tail, output_fingerprint, previous)
    }

    fn extract_detail(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<AgentDetail>> {
        reuse_detail_arc(extract_opencode_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_opencode_output_excerpt(output_tail), previous)
    }
}
