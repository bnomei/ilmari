//! Scaffold for Aider support.
//! Tracking issue: https://github.com/bnomei/ilmari/issues/12
//! Repository: https://github.com/Aider-AI/aider
//! Docs: https://aider.chat/docs/install.html

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{classify_supported_session, command_matches, AgentAdapter};

pub(in crate::agents) struct AiderAdapter;

impl AgentAdapter for AiderAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Aider
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "aider")
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
        _output_tail: Option<&str>,
        _previous: Option<&SessionRecord>,
    ) -> Option<Arc<AgentDetail>> {
        None
    }

    fn extract_output_excerpt(
        &self,
        _output_tail: Option<&str>,
        _previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        None
    }
}
