//! GitHub Copilot CLI adapter with custom waiting-prompt classification.
//!
//! Detects `copilot` command lines and recovers wrapped runs via title and output signals.
//! Repository: https://github.com/github/copilot-cli
//! Docs: https://docs.github.com/en/copilot/concepts/agents/copilot-cli/about-copilot-cli

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{
    classify_copilot_session, command_matches, extract_copilot_detail,
    extract_copilot_output_excerpt, is_shell_command, looks_like_copilot_output,
    pane_title_contains, reuse_detail_arc, reuse_output_excerpt_arc, AgentAdapter,
};

pub(in crate::agents) struct GitHubCopilotCliAdapter;

impl AgentAdapter for GitHubCopilotCliAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::GitHubCopilotCli
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "copilot")
            || (!is_shell_command(&pane.pane_current_command)
                && pane_title_contains(&pane.pane_title, "github copilot"))
    }

    fn detect_output(&self, _pane: &PaneSnapshot, output_tail: &str) -> bool {
        looks_like_copilot_output(output_tail)
    }

    fn classify(
        &self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        output_fingerprint: Option<u64>,
        previous: Option<&SessionRecord>,
    ) -> SessionStatus {
        classify_copilot_session(self, pane, output_tail, output_fingerprint, previous)
    }

    fn extract_detail(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<AgentDetail>> {
        reuse_detail_arc(extract_copilot_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_copilot_output_excerpt(output_tail), previous)
    }
}
