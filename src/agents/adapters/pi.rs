use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{
    classify_pi_session, command_equals_any, extract_pi_detail, extract_pi_output_excerpt,
    pane_title_contains, reuse_detail_arc, reuse_output_excerpt_arc, AgentAdapter,
};

pub(in crate::agents) struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Pi
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_equals_any(&pane.pane_current_command, &["pi", "pi-agent"])
            || pane.pane_title.contains('π')
            || pane_title_contains(&pane.pane_title, "pi v")
    }

    fn classify(
        &self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        output_fingerprint: Option<u64>,
        previous: Option<&SessionRecord>,
    ) -> SessionStatus {
        classify_pi_session(self, pane, output_tail, output_fingerprint, previous)
    }

    fn extract_detail(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<AgentDetail>> {
        reuse_detail_arc(extract_pi_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_pi_output_excerpt(output_tail), previous)
    }
}
