//! Provider-neutral tmux badge and compact status publication.
//!
//! The daemon writes pane-local attention badges and a global status summary so
//! status-lines and scripts stay agent-agnostic. Waiting and finished attention
//! latch until the pane is focused (or the session leaves that state), so a brief
//! glance at the badge is enough after a popup is closed.

use std::collections::{HashMap, HashSet};

use crate::config::{RendererConfig, StateFormat};
use crate::model::{SessionRecord, SessionStatus};
use crate::tmux;

/// Resolved badge and compact-status templates after TOML style/symbol merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderSettings {
    pub badges_enabled: bool,
    pub status_enabled: bool,
    /// Whether badges include the same agent identity shown by the popup's app column.
    pub show_agent_names: bool,
    pub badge_running: StateFormat,
    pub badge_waiting: StateFormat,
    pub badge_finished: StateFormat,
    pub status_running: StateFormat,
    pub status_waiting: StateFormat,
    pub status_finished: StateFormat,
    pub badge_separator: String,
    pub status_separator: String,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            badges_enabled: true,
            status_enabled: true,
            show_agent_names: false,
            badge_running: StateFormat { symbol: "●".to_string(), style: "fg=blue".to_string() },
            badge_waiting: StateFormat {
                symbol: "◆".to_string(), style: "fg=yellow".to_string()
            },
            badge_finished: StateFormat {
                symbol: "✓".to_string(), style: "fg=green".to_string()
            },
            status_running: StateFormat { symbol: "●".to_string(), style: "fg=blue".to_string() },
            status_waiting: StateFormat {
                symbol: "◆".to_string(), style: "fg=yellow".to_string()
            },
            status_finished: StateFormat {
                symbol: "✓".to_string(), style: "fg=green".to_string()
            },
            badge_separator: " ".to_string(),
            status_separator: " ".to_string(),
        }
    }
}

impl RenderSettings {
    /// Build pane badge and global status templates from the two renderer config blocks.
    pub fn from_config(
        badges: &RendererConfig,
        status: &RendererConfig,
        show_agent_names: bool,
    ) -> Self {
        Self {
            badges_enabled: badges.enabled,
            status_enabled: status.enabled,
            show_agent_names,
            badge_running: badges.running.clone(),
            badge_waiting: badges.waiting_input.clone(),
            badge_finished: badges.finished.clone(),
            status_running: status.running.clone(),
            status_waiting: status.waiting_input.clone(),
            status_finished: status.finished.clone(),
            badge_separator: badges.separator.clone(),
            status_separator: status.separator.clone(),
        }
    }
}

/// Aggregated counts used by the global compact status fragment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusCounts {
    pub running: usize,
    pub waiting: usize,
    pub finished: usize,
}

/// Per-pane attention latch for waiting/finished until focus acknowledges them.
///
/// `pending_*` holds transitions observed while focus probes are unavailable so a
/// brief scan glitch does not drop the badge when focus state returns.
#[derive(Debug, Clone, Copy, Default)]
struct PaneAttention {
    last_status: Option<SessionStatus>,
    waiting: bool,
    finished: bool,
    pending_waiting: bool,
    pending_finished: bool,
}

/// Incremental publisher of pane badges and the `@ilmari_status_summary` option.
#[derive(Debug, Default)]
pub struct TmuxStatePublisher {
    panes: HashMap<String, PaneAttention>,
    last_sessions: HashMap<String, SessionRecord>,
    current_session_ids: HashSet<String>,
    previously_published: HashSet<String>,
    badges_enabled: Option<bool>,
    status_enabled: Option<bool>,
}

impl TmuxStatePublisher {
    /// Diff live sessions against prior attention state and rewrite tmux options as needed.
    ///
    /// Side effects: pane-local badge options and the global status summary, gated by
    /// `RenderSettings` enable flags. Stale badges for dead panes are cleared.
    pub fn publish(
        &mut self,
        sessions: &[SessionRecord],
        live_panes: &[tmux::PaneSnapshot],
        settings: &RenderSettings,
    ) {
        let live_pane_ids =
            live_panes.iter().map(|pane| pane.pane_id.clone()).collect::<HashSet<_>>();
        let focused = tmux::focused_pane_ids();
        let live_sessions = sessions
            .iter()
            .filter(|session| live_pane_ids.contains(&session.pane.pane_id))
            .cloned()
            .collect::<Vec<_>>();
        self.panes.retain(|pane_id, _| live_pane_ids.contains(pane_id));
        self.last_sessions.retain(|pane_id, _| live_pane_ids.contains(pane_id));
        for session in &live_sessions {
            self.last_sessions.insert(session.pane.pane_id.clone(), session.clone());
        }
        for pane in live_panes {
            if let Some(session) = self.last_sessions.get_mut(&pane.pane_id) {
                session.pane = pane.clone();
            }
        }
        self.current_session_ids =
            live_sessions.iter().map(|session| session.pane.pane_id.clone()).collect();
        match focused {
            Ok(focused) => self.update_attention(&live_sessions, &focused, true),
            Err(_) => self.update_attention(&live_sessions, &HashSet::new(), false),
        }
        let render_sessions = self.sessions_for_render();
        let rendered = self.render(&render_sessions, settings);
        self.publish_rendered(&rendered, &live_pane_ids, settings);
    }

    /// Clear latched waiting/finished attention when a pane is focused between full refreshes.
    ///
    /// Also reapplies live `@ilmari_*_enabled` overrides so status-line toggles take effect
    /// without waiting for the next pane scan.
    pub fn acknowledge_focus(&mut self, settings: &RenderSettings) {
        let Ok(state) = tmux::focus_and_renderer_overrides() else {
            return;
        };
        let changed = self.resolve_known_focus(&state.focused_pane_ids);
        let badges_enabled = option_override_value(state.badges_enabled.as_deref())
            .unwrap_or(settings.badges_enabled);
        let status_enabled = option_override_value(state.status_enabled.as_deref())
            .unwrap_or(settings.status_enabled);
        let rendering_changed = self.badges_enabled != Some(badges_enabled)
            || self.status_enabled != Some(status_enabled);
        if !changed && !rendering_changed {
            return;
        }
        let sessions = self.sessions_for_render();
        let rendered = self.render(&sessions, settings);
        let live_pane_ids = self.previously_published.clone();
        self.publish_rendered(&rendered, &live_pane_ids, settings);
    }

    fn sessions_for_render(&self) -> Vec<SessionRecord> {
        let mut sessions = self
            .last_sessions
            .iter()
            .filter_map(|(pane_id, session)| {
                let attention = self.panes.get(pane_id).copied().unwrap_or_default();
                (self.current_session_ids.contains(pane_id)
                    || attention.waiting
                    || attention.finished)
                    .then(|| session.clone())
            })
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.pane.pane_id.cmp(&right.pane.pane_id));
        sessions
    }

    fn publish_rendered(
        &mut self,
        rendered: &RenderedState,
        live_pane_ids: &HashSet<String>,
        settings: &RenderSettings,
    ) {
        for pane_id in self.previously_published.difference(live_pane_ids) {
            let _ = tmux::set_pane_option(pane_id, "@ilmari_state", None);
            let _ = tmux::set_pane_option(pane_id, "@ilmari_badge", None);
        }
        for pane_id in live_pane_ids {
            if let Some((state, badge)) = rendered.pane_values.get(pane_id) {
                let _ = tmux::set_pane_option(pane_id, "@ilmari_state", Some(state));
                let _ = tmux::set_pane_option(pane_id, "@ilmari_badge", Some(badge));
            } else {
                let _ = tmux::set_pane_option(pane_id, "@ilmari_state", None);
                let _ = tmux::set_pane_option(pane_id, "@ilmari_badge", None);
            }
        }

        let badges_enabled =
            option_override("@ilmari_badges_enabled").unwrap_or(settings.badges_enabled);
        let status_enabled =
            option_override("@ilmari_status_enabled").unwrap_or(settings.status_enabled);
        self.badges_enabled = Some(badges_enabled);
        self.status_enabled = Some(status_enabled);
        let _ = tmux::set_global_option(
            "@ilmari_window_badges",
            if badges_enabled { &rendered.window_fragment } else { "" },
        );
        let _ = tmux::set_global_option(
            "@ilmari_status_summary",
            if status_enabled { &rendered.status_summary } else { "" },
        );
        let _ =
            tmux::set_global_option("@ilmari_running_count", &rendered.counts.running.to_string());
        let _ =
            tmux::set_global_option("@ilmari_waiting_count", &rendered.counts.waiting.to_string());
        let _ = tmux::set_global_option(
            "@ilmari_finished_count",
            &rendered.counts.finished.to_string(),
        );
        self.previously_published = live_pane_ids.clone();
    }

    /// Latch waiting/finished on status transitions for unfocused panes.
    ///
    /// When `allow_new_attention` is false (focus probe failed), transitions go to
    /// pending bits instead of live badges so focus recovery can promote them.
    fn update_attention(
        &mut self,
        sessions: &[SessionRecord],
        focused: &HashSet<String>,
        allow_new_attention: bool,
    ) {
        if allow_new_attention {
            self.resolve_known_focus(focused);
        }

        for session in sessions {
            let attention = self.panes.entry(session.pane.pane_id.clone()).or_default();
            let transitioned = attention.last_status.is_some_and(|last| last != session.status);
            if transitioned && !focused.contains(&session.pane.pane_id) {
                match session.status {
                    SessionStatus::WaitingInput if allow_new_attention => attention.waiting = true,
                    SessionStatus::WaitingInput => attention.pending_waiting = true,
                    SessionStatus::Finished if allow_new_attention => attention.finished = true,
                    SessionStatus::Finished => attention.pending_finished = true,
                    _ => {}
                }
            }
            attention.last_status = Some(session.status);
        }
    }

    /// Clear latched attention for focused panes; promote pending bits for others.
    fn resolve_known_focus(&mut self, focused: &HashSet<String>) -> bool {
        let mut changed = false;
        for (pane_id, attention) in &mut self.panes {
            if focused.contains(pane_id) {
                changed |= attention.waiting
                    || attention.finished
                    || attention.pending_waiting
                    || attention.pending_finished;
                attention.waiting = false;
                attention.finished = false;
                attention.pending_waiting = false;
                attention.pending_finished = false;
            } else {
                changed |= attention.pending_waiting || attention.pending_finished;
                attention.waiting |= attention.pending_waiting;
                attention.finished |= attention.pending_finished;
                attention.pending_waiting = false;
                attention.pending_finished = false;
            }
        }
        changed
    }

    /// Build pane badge strings and global status counts from latched attention.
    fn render(&self, sessions: &[SessionRecord], settings: &RenderSettings) -> RenderedState {
        let mut counts = StatusCounts::default();
        let mut windows: HashMap<&str, Vec<String>> = HashMap::new();
        let mut pane_values = HashMap::new();

        for session in sessions {
            let attention = self.panes.get(&session.pane.pane_id).copied().unwrap_or_default();
            let (state, format) = if attention.waiting {
                counts.waiting += 1;
                ("waiting-input", Some(&settings.badge_waiting))
            } else if attention.finished {
                counts.finished += 1;
                ("finished", Some(&settings.badge_finished))
            } else if session.status == SessionStatus::Running {
                counts.running += 1;
                ("running", Some(&settings.badge_running))
            } else {
                (session.status.as_str(), None)
            };
            let badge = format
                .map(|format| render_badge(format, session, settings.show_agent_names))
                .unwrap_or_default();
            if !badge.is_empty() {
                windows.entry(&session.pane.window_id).or_default().push(badge.clone());
            }
            pane_values.insert(session.pane.pane_id.clone(), (state.to_string(), badge));
        }

        let mut window_ids = windows.keys().copied().collect::<Vec<_>>();
        window_ids.sort_unstable();
        let window_fragment = window_ids
            .into_iter()
            .map(|window_id| {
                let badges = windows
                    .get(window_id)
                    .cloned()
                    .unwrap_or_default()
                    .join(&settings.badge_separator)
                    .replace(',', "#,");
                "#{?#{==:#{window_id},WINDOW},BADGES,}"
                    .replace("WINDOW", window_id)
                    .replace("BADGES", &badges)
            })
            .collect::<String>();

        let mut status_parts = Vec::new();
        if counts.waiting > 0 {
            status_parts.push(render_count(&settings.status_waiting, counts.waiting));
        }
        if counts.finished > 0 {
            status_parts.push(render_count(&settings.status_finished, counts.finished));
        }
        if counts.running > 0 {
            status_parts.push(render_count(&settings.status_running, counts.running));
        }

        RenderedState {
            counts,
            pane_values,
            window_fragment,
            status_summary: status_parts.join(&settings.status_separator),
        }
    }
}

struct RenderedState {
    counts: StatusCounts,
    pane_values: HashMap<String, (String, String)>,
    window_fragment: String,
    status_summary: String,
}

fn render_badge(format: &StateFormat, session: &SessionRecord, show_agent_names: bool) -> String {
    let label = if show_agent_names { session.kind.display_name() } else { "" };
    styled(&format.style, &format.symbol, label)
}

fn render_count(format: &StateFormat, count: usize) -> String {
    styled(&format.style, &format.symbol, &count.to_string())
}

fn styled(style: &str, symbol: &str, suffix: &str) -> String {
    let style = style.trim();
    let prefix = if style.is_empty() { String::new() } else { format!("#[{style}]") };
    if suffix.is_empty() {
        format!("{prefix}{symbol}#[default]")
    } else {
        format!("{prefix}{symbol} {suffix}#[default]")
    }
}

fn option_override(name: &str) -> Option<bool> {
    option_override_value(tmux::global_option(name).ok().as_deref())
}

fn option_override_value(value: Option<&str>) -> Option<bool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" => Some(true),
        "0" | "off" | "false" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{RenderSettings, TmuxStatePublisher};
    use crate::model::{AgentKind, SessionRecord, SessionStatus};
    use crate::tmux::PaneSnapshot;
    use std::collections::HashSet;
    use std::time::Instant;

    #[test]
    fn sticky_attention_acknowledges_exact_focused_pane_and_does_not_recreate_unchanged() {
        let mut publisher = TmuxStatePublisher::default();
        let running = session("%1", "@1", AgentKind::Codex, SessionStatus::Running);
        publisher.update_attention(std::slice::from_ref(&running), &HashSet::new(), true);
        let waiting = session("%1", "@1", AgentKind::Codex, SessionStatus::WaitingInput);
        publisher.update_attention(std::slice::from_ref(&waiting), &HashSet::new(), true);
        assert_eq!(
            publisher
                .render(std::slice::from_ref(&waiting), &RenderSettings::default())
                .counts
                .waiting,
            1
        );

        publisher.update_attention(
            std::slice::from_ref(&waiting),
            &HashSet::from(["%1".to_string()]),
            true,
        );
        assert_eq!(
            publisher
                .render(std::slice::from_ref(&waiting), &RenderSettings::default())
                .counts
                .waiting,
            0
        );
        publisher.update_attention(std::slice::from_ref(&waiting), &HashSet::new(), true);
        assert_eq!(
            publisher
                .render(std::slice::from_ref(&waiting), &RenderSettings::default())
                .counts
                .waiting,
            0
        );
    }

    #[test]
    fn pending_waiting_and_finished_are_acknowledged_when_focus_recovers_on_the_pane() {
        for status in [SessionStatus::WaitingInput, SessionStatus::Finished] {
            let mut publisher = TmuxStatePublisher::default();
            let running = session("%1", "@1", AgentKind::Codex, SessionStatus::Running);
            publisher.update_attention(std::slice::from_ref(&running), &HashSet::new(), true);

            let transitioned = session("%1", "@1", AgentKind::Codex, status);
            publisher.update_attention(std::slice::from_ref(&transitioned), &HashSet::new(), false);
            assert_eq!(attention_count(&publisher, &transitioned, status), 0);
            assert!(has_pending(&publisher, "%1", status));

            let focused = HashSet::from(["%1".to_string()]);
            publisher.update_attention(std::slice::from_ref(&transitioned), &focused, true);
            assert_eq!(attention_count(&publisher, &transitioned, status), 0);
            assert!(!has_pending(&publisher, "%1", status));

            // The acknowledged unchanged state must not recreate attention.
            publisher.update_attention(std::slice::from_ref(&transitioned), &HashSet::new(), true);
            assert_eq!(attention_count(&publisher, &transitioned, status), 0);

            // A later legitimate transition still creates attention.
            publisher.update_attention(std::slice::from_ref(&running), &HashSet::new(), true);
            publisher.update_attention(std::slice::from_ref(&transitioned), &HashSet::new(), true);
            assert_eq!(attention_count(&publisher, &transitioned, status), 1);
        }
    }

    #[test]
    fn pending_waiting_and_finished_become_sticky_when_focus_recovers_elsewhere() {
        for status in [SessionStatus::WaitingInput, SessionStatus::Finished] {
            let mut publisher = TmuxStatePublisher::default();
            let running = session("%1", "@1", AgentKind::Codex, SessionStatus::Running);
            publisher.update_attention(std::slice::from_ref(&running), &HashSet::new(), true);

            let transitioned = session("%1", "@1", AgentKind::Codex, status);
            publisher.update_attention(std::slice::from_ref(&transitioned), &HashSet::new(), false);
            assert_eq!(attention_count(&publisher, &transitioned, status), 0);
            assert!(has_pending(&publisher, "%1", status));

            publisher.update_attention(
                std::slice::from_ref(&transitioned),
                &HashSet::from(["%2".to_string()]),
                true,
            );
            assert_eq!(attention_count(&publisher, &transitioned, status), 1);
            assert!(!has_pending(&publisher, "%1", status));

            // Neither another failed lookup nor an unchanged successful scan acknowledges it.
            publisher.update_attention(std::slice::from_ref(&transitioned), &HashSet::new(), false);
            assert_eq!(attention_count(&publisher, &transitioned, status), 1);
            publisher.update_attention(
                std::slice::from_ref(&transitioned),
                &HashSet::from(["%2".to_string()]),
                true,
            );
            assert_eq!(attention_count(&publisher, &transitioned, status), 1);

            publisher.update_attention(
                std::slice::from_ref(&transitioned),
                &HashSet::from(["%1".to_string()]),
                true,
            );
            assert_eq!(attention_count(&publisher, &transitioned, status), 0);
            publisher.update_attention(std::slice::from_ref(&transitioned), &HashSet::new(), true);
            assert_eq!(attention_count(&publisher, &transitioned, status), 0);
        }
    }

    #[test]
    fn finished_attention_retains_metadata_after_agent_record_disappears_until_focus() {
        let mut publisher = TmuxStatePublisher::default();
        let running = session("%1", "@1", AgentKind::Codex, SessionStatus::Running);
        publisher.last_sessions.insert("%1".to_string(), running.clone());
        publisher.update_attention(std::slice::from_ref(&running), &HashSet::new(), true);
        let finished = session("%1", "@1", AgentKind::Codex, SessionStatus::Finished);
        publisher.last_sessions.insert("%1".to_string(), finished.clone());
        publisher.update_attention(std::slice::from_ref(&finished), &HashSet::new(), true);

        publisher.current_session_ids.clear();
        let retained = publisher.sessions_for_render();
        assert_eq!(retained.len(), 1);
        assert_eq!(publisher.render(&retained, &RenderSettings::default()).counts.finished, 1);

        publisher.update_attention(&[], &HashSet::from(["%1".to_string()]), true);
        assert!(publisher.sessions_for_render().is_empty());
    }

    #[test]
    fn aggregates_multiple_providers_in_one_window_with_priority_counts() {
        let mut publisher = TmuxStatePublisher::default();
        let initial = vec![
            session("%1", "@1", AgentKind::Codex, SessionStatus::Running),
            session("%2", "@1", AgentKind::ClaudeCode, SessionStatus::Running),
        ];
        publisher.update_attention(&initial, &HashSet::new(), true);
        let changed = vec![
            session("%1", "@1", AgentKind::Codex, SessionStatus::WaitingInput),
            session("%2", "@1", AgentKind::ClaudeCode, SessionStatus::Running),
        ];
        publisher.update_attention(&changed, &HashSet::new(), true);
        let settings = RenderSettings { show_agent_names: true, ..RenderSettings::default() };
        let rendered = publisher.render(&changed, &settings);
        assert_eq!(rendered.counts.waiting, 1);
        assert_eq!(rendered.counts.running, 1);
        assert!(rendered.window_fragment.contains("Codex"));
        assert!(rendered.window_fragment.contains("Claude Code"));
        assert!(
            rendered.status_summary.find("◆").unwrap() < rendered.status_summary.find("●").unwrap()
        );
    }

    #[test]
    fn window_badges_omit_agent_names_when_the_app_column_is_hidden() {
        let mut publisher = TmuxStatePublisher::default();
        let initial = vec![
            session("%1", "@1", AgentKind::Codex, SessionStatus::Running),
            session("%2", "@1", AgentKind::ClaudeCode, SessionStatus::Running),
        ];
        publisher.update_attention(&initial, &HashSet::new(), true);
        let changed = vec![
            session("%1", "@1", AgentKind::Codex, SessionStatus::WaitingInput),
            session("%2", "@1", AgentKind::ClaudeCode, SessionStatus::Running),
        ];
        publisher.update_attention(&changed, &HashSet::new(), true);

        let hidden = publisher.render(&changed, &RenderSettings::default());
        assert!(hidden.window_fragment.contains("◆"));
        assert!(hidden.window_fragment.contains("●"));
        assert!(!hidden.window_fragment.contains("Codex"));
        assert!(!hidden.window_fragment.contains("Claude Code"));

        let visible = publisher.render(
            &changed,
            &RenderSettings { show_agent_names: true, ..RenderSettings::default() },
        );
        assert!(visible.window_fragment.contains("Codex"));
        assert!(visible.window_fragment.contains("Claude Code"));
    }

    fn session(
        pane_id: &str,
        window_id: &str,
        kind: AgentKind,
        status: SessionStatus,
    ) -> SessionRecord {
        let now = Instant::now();
        SessionRecord {
            pane: PaneSnapshot::parse(&format!(
                "{pane_id}\t1\t$1\tdev\t{window_id}\tagents\t0\t/tmp\tcodex\ttitle"
            ))
            .unwrap(),
            kind,
            status,
            detail: None,
            output_excerpt: None,
            process_usage: None,
            output_fingerprint: None,
            last_changed_at: now,
            last_seen_at: now,
            retained_until: None,
        }
    }

    fn attention_count(
        publisher: &TmuxStatePublisher,
        session: &SessionRecord,
        status: SessionStatus,
    ) -> usize {
        let counts =
            publisher.render(std::slice::from_ref(session), &RenderSettings::default()).counts;
        match status {
            SessionStatus::WaitingInput => counts.waiting,
            SessionStatus::Finished => counts.finished,
            _ => unreachable!("helper only accepts attention statuses"),
        }
    }

    fn has_pending(publisher: &TmuxStatePublisher, pane_id: &str, status: SessionStatus) -> bool {
        let attention = publisher.panes.get(pane_id).expect("pane attention should exist");
        match status {
            SessionStatus::WaitingInput => attention.pending_waiting,
            SessionStatus::Finished => attention.pending_finished,
            _ => unreachable!("helper only accepts attention statuses"),
        }
    }
}
