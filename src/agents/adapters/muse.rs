//! Meta Muse CLI adapter based on observed Muse Code terminal states.

use std::sync::Arc;

use crate::model::{AgentDetail, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

use super::super::{
    classify_muse_session, command_matches, extract_muse_detail, extract_muse_output_excerpt,
    looks_like_muse_output, reuse_detail_arc, reuse_output_excerpt_arc, AgentAdapter,
};

/// Enabled Muse Code adapter (`muse` and opaque-suffixed `muse-*` binaries).
pub(in crate::agents) struct MuseAdapter;

impl AgentAdapter for MuseAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Muse
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "muse")
    }

    fn detect_output(&self, _pane: &PaneSnapshot, output_tail: &str) -> bool {
        looks_like_muse_output(output_tail)
    }

    fn classify(
        &self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        output_fingerprint: Option<u64>,
        previous: Option<&SessionRecord>,
    ) -> SessionStatus {
        classify_muse_session(self, pane, output_tail, output_fingerprint, previous)
    }

    fn extract_detail(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<AgentDetail>> {
        reuse_detail_arc(extract_muse_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_muse_output_excerpt(output_tail), previous)
    }
}
