//! Planned Cursor CLI adapter scaffold (disabled in the v1 registry).
//!
//! Tracking issue: https://github.com/bnomei/ilmari/issues/11
//! Repository: https://github.com/cursor/cursor
//! Docs: https://cursor.com/docs/cli/overview

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{classify_supported_session, command_matches, AgentAdapter};

pub(in crate::agents) struct CursorCliAdapter;

impl AgentAdapter for CursorCliAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::CursorCli
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "cursor")
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
