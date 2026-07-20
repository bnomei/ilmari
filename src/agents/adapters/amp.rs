//! Amp adapter: detects `amp` panes and classifies mode-specific waiting prompts.
//! Repository: https://github.com/sourcegraph/amp
//! Docs: https://ampcode.com/manual

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{
    classify_amp_session, command_matches, extract_amp_detail, extract_amp_output_excerpt,
    reuse_detail_arc, reuse_output_excerpt_arc, AgentAdapter,
};

/// Enabled Amp CLI adapter (`amp`).
pub(in crate::agents) struct AmpAdapter;

impl AgentAdapter for AmpAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Amp
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "amp")
    }

    fn classify(
        &self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        output_fingerprint: Option<u64>,
        previous: Option<&SessionRecord>,
    ) -> SessionStatus {
        classify_amp_session(self, pane, output_tail, output_fingerprint, previous)
    }

    fn extract_detail(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<AgentDetail>> {
        reuse_detail_arc(extract_amp_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_amp_output_excerpt(output_tail), previous)
    }
}
