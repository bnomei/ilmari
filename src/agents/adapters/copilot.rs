//! Scaffold for GitHub Copilot CLI support.
//! Tracking issue: https://github.com/bnomei/ilmari/issues/10
//! Repository: https://github.com/github/copilot-cli
//! Docs: https://docs.github.com/en/copilot/concepts/agents/copilot-cli/about-copilot-cli

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{classify_supported_session, command_matches, AgentAdapter};

pub(in crate::agents) struct GitHubCopilotCliAdapter;

impl AgentAdapter for GitHubCopilotCliAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::GitHubCopilotCli
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "copilot")
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
