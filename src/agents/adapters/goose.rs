//! Planned Goose CLI adapter scaffold (disabled in the v1 registry).
//!
//! Tracking issue: https://github.com/bnomei/ilmari/issues/14
//! Repository: https://github.com/aaif-goose/goose
//! Docs: https://goose-docs.ai/docs/getting-started/installation/

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{classify_supported_session, command_matches, AgentAdapter};

/// Planned Goose CLI adapter scaffold; filtered out of `AdapterRegistry::v1`.
pub(in crate::agents) struct GooseCliAdapter;

impl AgentAdapter for GooseCliAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::GooseCli
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "goose")
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
