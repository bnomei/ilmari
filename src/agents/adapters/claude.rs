//! Claude Code adapter with title and output-tail fallbacks for wrapped launches.
//! Repository: https://github.com/anthropics/claude-code
//! Docs: https://docs.anthropic.com/en/docs/claude-code

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{
    classify_supported_session, command_matches, command_matches_non_claude_agent,
    extract_claude_detail, extract_claude_output_excerpt, is_claude_direct_title,
    is_claude_spinner_title, is_shell_command, looks_like_claude_output, reuse_detail_arc,
    reuse_output_excerpt_arc, AgentAdapter,
};

/// Enabled Claude Code adapter (`claude`) with wrapped-launch fallbacks.
pub(in crate::agents) struct ClaudeCodeAdapter;

impl AgentAdapter for ClaudeCodeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "claude")
            || (!is_shell_command(&pane.pane_current_command)
                && !command_matches_non_claude_agent(&pane.pane_current_command)
                && is_claude_direct_title(&pane.pane_title))
    }

    fn detect_output(&self, pane: &PaneSnapshot, output_tail: &str) -> bool {
        !is_shell_command(&pane.pane_current_command)
            && !command_matches_non_claude_agent(&pane.pane_current_command)
            && is_claude_spinner_title(&pane.pane_title)
            && looks_like_claude_output(output_tail)
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
        reuse_detail_arc(extract_claude_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_claude_output_excerpt(output_tail), previous)
    }
}
