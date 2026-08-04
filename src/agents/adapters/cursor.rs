//! Cursor CLI adapter with output-tail recovery for runtime-wrapped sessions.
//!
//! Tracking issue: https://github.com/bnomei/ilmari/issues/11
//! Repository: https://github.com/cursor/cursor
//! Docs: https://cursor.com/docs/cli/overview

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{
    classify_cursor_session, command_matches, extract_cursor_detail, extract_cursor_output_excerpt,
    looks_like_cursor_output, reuse_detail_arc, reuse_output_excerpt_arc, AgentAdapter,
};

/// Enabled Cursor CLI adapter (`cursor` / `cursor-agent`).
pub(in crate::agents) struct CursorCliAdapter;

impl AgentAdapter for CursorCliAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::CursorCli
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "cursor")
    }

    fn detect_output(&self, _pane: &PaneSnapshot, output_tail: &str) -> bool {
        looks_like_cursor_output(output_tail)
    }

    fn classify(
        &self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        output_fingerprint: Option<u64>,
        previous: Option<&SessionRecord>,
    ) -> SessionStatus {
        classify_cursor_session(self, pane, output_tail, output_fingerprint, previous)
    }

    fn extract_detail(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<AgentDetail>> {
        reuse_detail_arc(extract_cursor_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_cursor_output_excerpt(output_tail), previous)
    }
}
