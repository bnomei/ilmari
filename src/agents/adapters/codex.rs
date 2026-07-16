//! OpenAI Codex CLI adapter: detects `codex` panes and extracts model detail.
//!
//! Uses the shared supported-session classifier and output-excerpt path.
//! Repository: https://github.com/openai/codex
//! Docs: https://developers.openai.com/codex/cli

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{
    classify_supported_session, command_matches, extract_codex_detail,
    extract_codex_output_excerpt, reuse_detail_arc, reuse_output_excerpt_arc, AgentAdapter,
};

/// Enabled Codex CLI adapter (`codex`).
pub(in crate::agents) struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "codex")
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
        reuse_detail_arc(extract_codex_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_codex_output_excerpt(output_tail), previous)
    }
}
