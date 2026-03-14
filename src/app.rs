use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::{DefaultTerminal, Frame};
use time::{OffsetDateTime, UtcOffset};

use crate::agents::SessionTracker;
use crate::colors::Palette;
use crate::git::{GitSummaryCache, GitSummaryReport};
use crate::model::{
    AgentKind, AppModel, GitSummaryRow, PaneRow, SessionProcessUsage, SessionRecord, SessionStatus,
    WorkspaceGroup,
};
use crate::process::{self, ProcessTree};
use crate::sound;
use crate::tmux;
use crate::ui;

const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_PROCESS_REFRESH_INTERVAL: Duration = Duration::from_secs(15);
pub fn run() -> Result<()> {
    let mut terminal = ratatui::init();
    let _guard = TerminalGuard;
    let result = run_app(&mut terminal);
    ratatui::restore();
    result
}

fn run_app(terminal: &mut DefaultTerminal) -> Result<()> {
    let mut app = App::from_env();
    app.refresh(true);
    let mut needs_redraw = true;

    while !app.should_quit {
        if needs_redraw {
            terminal.draw(|frame| app.draw(frame))?;
            needs_redraw = false;
        }

        if event::poll(app.poll_timeout())? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    needs_redraw |= app.handle_key_event(key.code, key.modifiers);
                }
            }
        }

        if !app.should_quit && app.refresh_due() {
            app.refresh(false);
            needs_redraw = true;
        }
    }

    Ok(())
}

struct App {
    model: AppModel,
    status_line: String,
    git_summaries: Vec<GitSummaryRow>,
    palette: Palette,
    should_quit: bool,
    quit_on_activate: bool,
    display_offset: UtcOffset,
    session_tracker: SessionTracker,
    git_cache: GitSummaryCache,
    process_cache: ProcessUsageCache,
    sessions: Vec<SessionRecord>,
    selected_pane_id: Option<String>,
    expanded_pane_ids: HashSet<String>,
    pane_jump_digits: Option<String>,
    show_app: bool,
    show_git: bool,
    show_detail: bool,
    show_time: bool,
    show_output: bool,
    show_stats: bool,
    show_app_pinned: bool,
    show_detail_pinned: bool,
    show_stats_pinned: bool,
    bell_enabled: bool,
    hydrated: bool,
}

struct CachedProcessTree {
    tree: ProcessTree,
    refreshed_at: Instant,
    usage_by_session: HashMap<ProcessUsageKey, Option<Arc<SessionProcessUsage>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ProcessUsageKey {
    pane_pid: Option<u32>,
    kind: AgentKind,
}

struct ProcessUsageCache {
    refresh_interval: Duration,
    cached_tree: Option<CachedProcessTree>,
}

impl CachedProcessTree {
    fn new(tree: ProcessTree, refreshed_at: Instant) -> Self {
        Self { tree, refreshed_at, usage_by_session: HashMap::new() }
    }

    fn usage_for_session(&mut self, session: &SessionRecord) -> Option<Arc<SessionProcessUsage>> {
        let key = ProcessUsageKey { pane_pid: session.pane.pane_pid, kind: session.kind };
        if let Some(usage) = self.usage_by_session.get(&key) {
            return usage.clone();
        }

        let usage = self.tree.usage_for_session(session).map(Arc::new);
        self.usage_by_session.insert(key, usage.clone());
        usage
    }
}

impl ProcessUsageCache {
    fn new(refresh_interval: Duration) -> Self {
        Self { refresh_interval, cached_tree: None }
    }

    fn hydrate(
        &mut self,
        sessions: &mut [SessionRecord],
        refreshed_at: Instant,
        enabled: bool,
    ) -> Result<(), process::ProcessError> {
        if !enabled {
            self.release(sessions);
            return Ok(());
        }

        if sessions.is_empty() {
            self.cached_tree = None;
            return Ok(());
        }

        self.ensure_tree(refreshed_at)?;

        if let Some(cached) = &mut self.cached_tree {
            for session in sessions {
                session.process_usage = cached.usage_for_session(session);
            }
        }

        Ok(())
    }

    fn hydrate_session(
        &mut self,
        session: &mut SessionRecord,
        refreshed_at: Instant,
    ) -> Result<(), process::ProcessError> {
        self.ensure_tree(refreshed_at)?;
        if let Some(cached) = &mut self.cached_tree {
            session.process_usage = cached.usage_for_session(session);
        } else {
            session.process_usage = None;
        }
        Ok(())
    }

    fn ensure_tree(&mut self, refreshed_at: Instant) -> Result<(), process::ProcessError> {
        let should_refresh = match &self.cached_tree {
            Some(cached) => {
                refreshed_at.duration_since(cached.refreshed_at) >= self.refresh_interval
            }
            None => true,
        };

        if should_refresh {
            self.cached_tree =
                Some(CachedProcessTree::new(process::collect_process_tree()?, refreshed_at));
        }

        Ok(())
    }

    fn release(&mut self, sessions: &mut [SessionRecord]) {
        self.cached_tree = None;
        for session in sessions {
            session.process_usage = None;
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new_with_process_refresh(
            Palette::default(),
            DEFAULT_REFRESH_INTERVAL,
            DEFAULT_PROCESS_REFRESH_INTERVAL,
            false,
        )
    }
}

impl App {
    fn from_env() -> Self {
        Self::new_with_process_refresh(
            Palette::from_env(),
            refresh_interval_from_env(),
            process_refresh_interval_from_env(),
            quit_on_activate_from_env(),
        )
    }

    #[cfg(test)]
    fn new(palette: Palette, refresh_interval: Duration, quit_on_activate: bool) -> Self {
        Self::new_with_process_refresh(
            palette,
            refresh_interval,
            DEFAULT_PROCESS_REFRESH_INTERVAL,
            quit_on_activate,
        )
    }

    fn new_with_process_refresh(
        palette: Palette,
        refresh_interval: Duration,
        process_refresh_interval: Duration,
        quit_on_activate: bool,
    ) -> Self {
        let mut model = AppModel::placeholder();
        model.refresh_interval = refresh_interval;
        let status_line = model.status_line.clone();

        Self {
            model,
            status_line,
            git_summaries: Vec::new(),
            palette,
            should_quit: false,
            quit_on_activate,
            display_offset: display_utc_offset(),
            session_tracker: SessionTracker::new(),
            git_cache: GitSummaryCache::new(),
            process_cache: ProcessUsageCache::new(process_refresh_interval),
            sessions: Vec::new(),
            selected_pane_id: None,
            expanded_pane_ids: HashSet::new(),
            pane_jump_digits: None,
            show_app: false,
            show_git: true,
            show_detail: false,
            show_time: true,
            show_output: true,
            show_stats: false,
            show_app_pinned: false,
            show_detail_pinned: false,
            show_stats_pinned: false,
            bell_enabled: true,
            hydrated: false,
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        self.apply_responsive_view_defaults(available_render_width(frame));
        ui::render(frame, &self.model, &self.palette);
    }

    fn poll_timeout(&self) -> Duration {
        self.model.refresh_interval.saturating_sub(self.model.last_refresh.elapsed())
    }

    fn refresh_due(&self) -> bool {
        self.model.last_refresh.elapsed() >= self.model.refresh_interval
    }

    fn handle_key_event(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        match self.handle_pane_jump_key(code, modifiers) {
            PaneJumpKeyResult::Consumed { redraw } => redraw,
            PaneJumpKeyResult::Continue { redraw } => {
                let normal_redraw = match (code, modifiers) {
                    (KeyCode::Char('q'), _) => {
                        self.should_quit = true;
                        false
                    }
                    (KeyCode::Char('c'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                        self.should_quit = true;
                        false
                    }
                    (KeyCode::Char('%'), mods)
                        if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.pane_jump_digits = Some(String::new());
                        self.refresh_jump_matches();
                        true
                    }
                    (KeyCode::Char('a'), mods)
                        if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.toggle_show_app()
                    }
                    (KeyCode::Char('b'), mods)
                        if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.toggle_bell()
                    }
                    (KeyCode::Char('g'), mods)
                        if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.toggle_show_git()
                    }
                    (KeyCode::Char('m'), mods)
                        if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.toggle_show_detail()
                    }
                    (KeyCode::Char('t'), mods)
                        if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.toggle_show_time()
                    }
                    (KeyCode::Char('o'), mods)
                        if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.toggle_show_output()
                    }
                    (KeyCode::Char('s'), mods)
                        if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.toggle_show_stats()
                    }
                    (KeyCode::Char('j'), _) | (KeyCode::Down, _) => self.move_selection(1),
                    (KeyCode::Char('k'), _) | (KeyCode::Up, _) => self.move_selection(-1),
                    (KeyCode::Char('='), mods)
                        if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.toggle_selected_subtasks()
                    }
                    (KeyCode::Enter, _) => self.activate_selected(),
                    _ => false,
                };

                redraw || normal_redraw
            }
        }
    }

    fn handle_pane_jump_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> PaneJumpKeyResult {
        let Some(digits) = self.pane_jump_digits.as_mut() else {
            return PaneJumpKeyResult::Continue { redraw: false };
        };

        match code {
            KeyCode::Char(digit)
                if digit.is_ascii_digit()
                    && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                digits.push(digit);
                let target = format!("%{}", digits);
                self.select_exact_pane_id(&target);
                self.refresh_jump_matches();
                PaneJumpKeyResult::Consumed { redraw: true }
            }
            _ => {
                self.pane_jump_digits = None;
                self.refresh_jump_matches();
                PaneJumpKeyResult::Continue { redraw: true }
            }
        }
    }

    fn move_selection(&mut self, offset: isize) -> bool {
        let ordered: Vec<_> = visible_pane_ids(&self.model.workspace_groups).collect();
        if ordered.is_empty() {
            return false;
        }

        let current = self
            .selected_pane_id
            .as_ref()
            .and_then(|pane_id| ordered.iter().position(|candidate| *candidate == pane_id))
            .unwrap_or(if offset.is_negative() { ordered.len() - 1 } else { 0 });
        let next = (current as isize + offset).clamp(0, ordered.len() as isize - 1) as usize;
        self.update_selected_pane(Some(ordered[next].to_string()))
    }

    fn refresh(&mut self, force_git_refresh: bool) {
        let refreshed_at = Instant::now();
        let refreshed_at_wallclock = SystemTime::now();
        let previous_statuses = current_statuses(&self.sessions);

        match tmux::collect_pane_snapshots() {
            Ok(panes) => {
                let output_tails =
                    tmux::capture_output_tails(&panes, &self.session_tracker, refreshed_at);
                self.sessions = self.session_tracker.refresh(&panes, &output_tails, refreshed_at);
                let needs_process_usage = self.needs_process_usage();
                let process_warning = self
                    .process_cache
                    .hydrate(&mut self.sessions, refreshed_at, needs_process_usage)
                    .err()
                    .map(|error| format!("ps: {error}"));
                normalize_expanded_pane_ids(&mut self.expanded_pane_ids, &self.sessions);
                self.emit_bells(count_alert_transitions(&previous_statuses, &self.sessions));

                let (status_line, git_summaries) = if self.show_git {
                    let git_report = self.git_cache.summary_rows_for_workspaces(
                        self.sessions
                            .iter()
                            .map(|session| session.pane.pane_current_path.as_path()),
                        refreshed_at,
                        force_git_refresh,
                    );
                    (status_line(&git_report, process_warning.as_deref()), git_report.rows)
                } else {
                    (process_warning.unwrap_or_default(), Vec::new())
                };

                self.sync_model(status_line, git_summaries, refreshed_at, refreshed_at_wallclock);
            }
            Err(error) => {
                self.sessions.clear();
                self.selected_pane_id = None;
                self.expanded_pane_ids.clear();
                self.pane_jump_digits = None;
                self.sync_model(
                    format!("tmux snapshot failed: {error}"),
                    Vec::new(),
                    refreshed_at,
                    refreshed_at_wallclock,
                );
            }
        }
    }

    fn sync_model(
        &mut self,
        status_line: String,
        git_summaries: Vec<GitSummaryRow>,
        refreshed_at: Instant,
        refreshed_at_wallclock: SystemTime,
    ) {
        self.status_line = status_line;
        self.git_summaries = relabel_git_summaries(git_summaries);
        self.rebuild_model(refreshed_at, refreshed_at_wallclock);
    }

    fn rebuild_model(&mut self, refreshed_at: Instant, refreshed_at_wallclock: SystemTime) {
        let refresh_interval = self.model.refresh_interval;
        self.model = build_model_with_preferences(
            &self.sessions,
            &self.git_summaries,
            BuildModelOptions {
                selected_pane_id: self.selected_pane_id.as_deref(),
                expanded_pane_ids: &self.expanded_pane_ids,
                pane_jump_digits: self.pane_jump_digits.as_deref(),
                status_line: self.status_line.as_str(),
                show_app: self.show_app,
                show_git: self.show_git,
                show_detail: self.show_detail,
                show_time: self.show_time,
                show_output: self.show_output,
                show_stats: self.show_stats,
                refresh_interval,
                refreshed_at,
                refreshed_at_wallclock,
                display_offset: self.display_offset,
            },
        );

        let normalized = normalize_selected_pane_id(
            self.selected_pane_id.as_deref(),
            &self.model.workspace_groups,
            &self.sessions,
        );

        if normalized.as_deref() != self.selected_pane_id.as_deref() {
            self.selected_pane_id = normalized;
        }

        apply_selected_pane(&mut self.model.workspace_groups, self.selected_pane_id.as_deref());
    }

    fn refresh_jump_matches(&mut self) {
        apply_jump_matches(&mut self.model.workspace_groups, self.pane_jump_digits.as_deref());
    }

    fn refresh_view_preferences(&mut self) {
        self.model.show_app = self.show_app;
        self.model.show_git = self.show_git;
        self.model.show_detail = self.show_detail;
        self.model.show_time = self.show_time;
        self.model.show_output = self.show_output;
        self.model.show_stats = self.show_stats;
    }

    fn emit_bells(&mut self, transitions: usize) {
        if self.hydrated && self.bell_enabled {
            for _ in 0..transitions {
                let _ = sound::ring_terminal_bell();
            }
        }

        self.hydrated = true;
    }

    fn activate_selected(&mut self) -> bool {
        let Some(session) = self.selected_session() else {
            return false;
        };

        match tmux::jump_to_pane(&session.pane) {
            Ok(()) => {
                self.should_quit = self.quit_on_activate;
                false
            }
            Err(error) => {
                self.status_line = format!("pane activation failed: {error}");
                self.model.status_line = self.status_line.clone();
                true
            }
        }
    }

    fn toggle_selected_subtasks(&mut self) -> bool {
        let Some(pane_id) = self.selected_pane_id.clone() else {
            return false;
        };
        if !self.ensure_selected_process_usage() {
            return false;
        }
        let Some(process_usage) = self
            .sessions
            .iter()
            .find(|session| session.pane.pane_id == pane_id)
            .and_then(|session| session.process_usage.as_ref())
        else {
            return false;
        };
        if process_usage.subtasks.is_empty() {
            return false;
        }
        if self.expanded_pane_ids.contains(&pane_id) {
            self.expanded_pane_ids.remove(&pane_id);
        } else {
            self.expanded_pane_ids.insert(pane_id);
        }

        if !self.needs_process_usage() {
            self.process_cache.release(&mut self.sessions);
        }

        self.rebuild_model(Instant::now(), SystemTime::now());
        true
    }

    fn toggle_show_detail(&mut self) -> bool {
        self.show_detail = !self.show_detail;
        self.show_detail_pinned = true;
        self.refresh_view_preferences();
        true
    }

    fn toggle_show_app(&mut self) -> bool {
        self.show_app = !self.show_app;
        self.show_app_pinned = true;
        self.refresh_view_preferences();
        true
    }

    fn toggle_bell(&mut self) -> bool {
        self.bell_enabled = !self.bell_enabled;
        self.status_line = format!("bell: {}", if self.bell_enabled { "on" } else { "off" });
        self.model.status_line = self.status_line.clone();
        true
    }

    fn toggle_show_git(&mut self) -> bool {
        self.show_git = !self.show_git;
        let refreshed_at = Instant::now();
        if self.show_git {
            let git_report = self.git_cache.summary_rows_for_workspaces(
                self.sessions.iter().map(|session| session.pane.pane_current_path.as_path()),
                refreshed_at,
                true,
            );
            self.git_summaries = relabel_git_summaries(git_report.rows);
        } else {
            self.git_summaries.clear();
        }
        self.rebuild_model(refreshed_at, SystemTime::now());
        true
    }

    fn toggle_show_time(&mut self) -> bool {
        self.show_time = !self.show_time;
        self.refresh_view_preferences();
        true
    }

    fn toggle_show_output(&mut self) -> bool {
        self.show_output = !self.show_output;
        self.refresh_view_preferences();
        true
    }

    fn toggle_show_stats(&mut self) -> bool {
        self.show_stats = !self.show_stats;
        self.show_stats_pinned = true;
        let refreshed_at = Instant::now();
        let needs_process_usage = self.needs_process_usage();
        if let Err(error) =
            self.process_cache.hydrate(&mut self.sessions, refreshed_at, needs_process_usage)
        {
            self.status_line = format!("ps: {error}");
        }
        self.rebuild_model(refreshed_at, SystemTime::now());
        true
    }

    fn apply_responsive_view_defaults(&mut self, _available_width: usize) {
        let mut changed = false;

        if !self.show_app_pinned && self.show_app {
            self.show_app = false;
            changed = true;
        }

        if !self.show_detail_pinned && self.show_detail {
            self.show_detail = false;
            changed = true;
        }

        if !self.show_stats_pinned && self.show_stats {
            self.show_stats = false;
            changed = true;
        }

        if changed {
            self.refresh_view_preferences();
        }
    }

    fn select_exact_pane_id(&mut self, pane_id: &str) -> bool {
        if !contains_visible_pane_id(&self.model.workspace_groups, pane_id) {
            return false;
        }

        if self.selected_pane_id.as_deref() == Some(pane_id) {
            return false;
        }

        self.update_selected_pane(Some(pane_id.to_string()))
    }

    fn selected_session(&self) -> Option<&SessionRecord> {
        self.selected_pane_id.as_ref().and_then(|pane_id| {
            self.sessions.iter().find(|session| session.pane.pane_id == *pane_id)
        })
    }

    fn update_selected_pane(&mut self, pane_id: Option<String>) -> bool {
        if pane_id.as_deref() == self.selected_pane_id.as_deref() {
            return false;
        }

        self.selected_pane_id = pane_id;
        apply_selected_pane(&mut self.model.workspace_groups, self.selected_pane_id.as_deref());
        true
    }

    fn needs_process_usage(&self) -> bool {
        self.show_stats || !self.expanded_pane_ids.is_empty()
    }

    fn ensure_selected_process_usage(&mut self) -> bool {
        let Some(pane_id) = self.selected_pane_id.clone() else {
            return false;
        };
        let Some(index) = self.sessions.iter().position(|session| session.pane.pane_id == pane_id)
        else {
            return false;
        };
        if self.sessions[index].process_usage.is_some() {
            return true;
        }

        let refreshed_at = Instant::now();
        if let Err(error) =
            self.process_cache.hydrate_session(&mut self.sessions[index], refreshed_at)
        {
            self.status_line = format!("ps: {error}");
            self.model.status_line = self.status_line.clone();
            return false;
        }

        true
    }
}

enum PaneJumpKeyResult {
    Continue { redraw: bool },
    Consumed { redraw: bool },
}

struct BuildModelOptions<'a> {
    selected_pane_id: Option<&'a str>,
    expanded_pane_ids: &'a HashSet<String>,
    pane_jump_digits: Option<&'a str>,
    status_line: &'a str,
    show_app: bool,
    show_git: bool,
    show_detail: bool,
    show_time: bool,
    show_output: bool,
    show_stats: bool,
    refresh_interval: Duration,
    refreshed_at: Instant,
    refreshed_at_wallclock: SystemTime,
    display_offset: UtcOffset,
}

fn build_model_with_preferences(
    sessions: &[SessionRecord],
    git_summaries: &[GitSummaryRow],
    options: BuildModelOptions<'_>,
) -> AppModel {
    let workspace_labels =
        derive_path_labels(sessions.iter().map(|session| session.pane.pane_current_path.as_path()));

    struct GroupAccumulator<'a> {
        workspace_path: PathBuf,
        sessions: Vec<&'a SessionRecord>,
    }

    let mut grouped: BTreeMap<String, GroupAccumulator<'_>> = BTreeMap::new();
    for session in sessions {
        let label = workspace_labels
            .get(session.pane.pane_current_path.as_path())
            .cloned()
            .unwrap_or_else(|| session.pane.pane_current_path.display().to_string());
        grouped
            .entry(label)
            .or_insert_with(|| GroupAccumulator {
                workspace_path: session.pane.pane_current_path.clone(),
                sessions: Vec::new(),
            })
            .sessions
            .push(session);
    }

    let workspace_groups = grouped
        .into_iter()
        .map(|(label, mut group)| {
            group.sessions.sort_by(|left, right| compare_sessions_for_workspace(left, right));

            let git_summary =
                git_summary_for_workspace(&group.workspace_path, git_summaries).cloned();

            let rows = group
                .sessions
                .into_iter()
                .map(|session| PaneRow {
                    pane_id: session.pane.pane_id.clone(),
                    inactive_since_label: inactive_since_label(
                        session,
                        options.refreshed_at,
                        options.refreshed_at_wallclock,
                        options.display_offset,
                    ),
                    output_excerpt: session.output_excerpt.clone(),
                    client_label: session.kind.display_name(),
                    detail: session.detail.clone(),
                    process_usage: session.process_usage.clone(),
                    subtasks_expanded: options.expanded_pane_ids.contains(&session.pane.pane_id),
                    status: session.status,
                    status_label: session.status.as_str(),
                    is_jump_match: options
                        .pane_jump_digits
                        .filter(|digits| !digits.is_empty())
                        .is_some_and(|digits| {
                            session.pane.pane_id.trim_start_matches('%').starts_with(digits)
                        }),
                    is_selected: options.selected_pane_id == Some(session.pane.pane_id.as_str()),
                })
                .collect();

            WorkspaceGroup { label, git_summary, rows }
        })
        .collect();

    AppModel {
        title: "Agents".to_string(),
        status_line: options.status_line.to_string(),
        show_app: options.show_app,
        show_git: options.show_git,
        show_detail: options.show_detail,
        show_time: options.show_time,
        show_output: options.show_output,
        show_stats: options.show_stats,
        workspace_groups,
        refresh_interval: options.refresh_interval,
        last_refresh: options.refreshed_at,
        last_refresh_wallclock: options.refreshed_at_wallclock,
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn build_model(
    sessions: &[SessionRecord],
    git_summaries: Vec<GitSummaryRow>,
    selected_pane_id: Option<&str>,
    expanded_pane_ids: &HashSet<String>,
    pane_jump_digits: Option<&str>,
    status_line: String,
    refresh_interval: Duration,
    refreshed_at: Instant,
    refreshed_at_wallclock: SystemTime,
) -> AppModel {
    let git_summaries = relabel_git_summaries(git_summaries);
    build_model_with_preferences(
        sessions,
        &git_summaries,
        BuildModelOptions {
            selected_pane_id,
            expanded_pane_ids,
            pane_jump_digits,
            status_line: status_line.as_str(),
            show_app: false,
            show_git: true,
            show_detail: false,
            show_time: true,
            show_output: true,
            show_stats: false,
            refresh_interval,
            refreshed_at,
            refreshed_at_wallclock,
            display_offset: UtcOffset::UTC,
        },
    )
}

fn git_summary_for_workspace<'a>(
    workspace_path: &Path,
    git_summaries: &'a [GitSummaryRow],
) -> Option<&'a GitSummaryRow> {
    git_summaries
        .iter()
        .filter(|summary| workspace_path.starts_with(&summary.workspace_path))
        .max_by_key(|summary| summary.workspace_path.components().count())
}

fn relabel_git_summaries(mut git_summaries: Vec<GitSummaryRow>) -> Vec<GitSummaryRow> {
    let workspace_paths: Vec<PathBuf> =
        git_summaries.iter().map(|summary| summary.workspace_path.clone()).collect();
    let git_labels = derive_path_labels(workspace_paths.iter().map(|path| path.as_path()));

    for summary in &mut git_summaries {
        if let Some(label) = git_labels.get(summary.workspace_path.as_path()) {
            summary.workspace_label = label.clone();
        }
    }

    git_summaries
}

fn compare_sessions_for_workspace(left: &SessionRecord, right: &SessionRecord) -> Ordering {
    session_sort_bucket(left.status)
        .cmp(&session_sort_bucket(right.status))
        .then_with(|| compare_sessions_within_bucket(left, right))
}

fn session_sort_bucket(status: SessionStatus) -> u8 {
    match status {
        SessionStatus::Running => 0,
        SessionStatus::WaitingInput => 1,
        SessionStatus::Finished | SessionStatus::Terminated | SessionStatus::Unknown => 2,
    }
}

fn compare_sessions_within_bucket(left: &SessionRecord, right: &SessionRecord) -> Ordering {
    if left.status == SessionStatus::WaitingInput && right.status == SessionStatus::WaitingInput {
        return right
            .last_changed_at
            .cmp(&left.last_changed_at)
            .then_with(|| compare_pane_ids_desc(&left.pane.pane_id, &right.pane.pane_id));
    }

    compare_pane_ids_desc(&left.pane.pane_id, &right.pane.pane_id)
}

fn compare_pane_ids_desc(left: &str, right: &str) -> Ordering {
    match (pane_numeric_id(left), pane_numeric_id(right)) {
        (Some(left_id), Some(right_id)) => right_id.cmp(&left_id).then_with(|| right.cmp(left)),
        _ => right.cmp(left),
    }
}

fn pane_numeric_id(pane_id: &str) -> Option<u32> {
    pane_id.trim_start_matches('%').parse::<u32>().ok()
}

fn normalize_selected_pane_id(
    current: Option<&str>,
    workspace_groups: &[WorkspaceGroup],
    sessions: &[SessionRecord],
) -> Option<String> {
    if workspace_groups.is_empty() {
        return None;
    }

    current
        .filter(|pane_id| contains_visible_pane_id(workspace_groups, pane_id))
        .map(ToOwned::to_owned)
        .or_else(|| most_recent_waiting_pane_id(sessions))
        .or_else(|| first_visible_pane_id(workspace_groups).map(ToOwned::to_owned))
}

fn most_recent_waiting_pane_id(sessions: &[SessionRecord]) -> Option<String> {
    sessions
        .iter()
        .filter(|session| session.status == SessionStatus::WaitingInput)
        .max_by(|left, right| {
            left.last_changed_at.cmp(&right.last_changed_at).then_with(|| {
                pane_numeric_id(&left.pane.pane_id)
                    .cmp(&pane_numeric_id(&right.pane.pane_id))
                    .then_with(|| left.pane.pane_id.cmp(&right.pane.pane_id))
            })
        })
        .map(|session| session.pane.pane_id.clone())
}

fn apply_selected_pane(workspace_groups: &mut [WorkspaceGroup], selected_pane_id: Option<&str>) {
    for group in workspace_groups {
        for row in &mut group.rows {
            row.is_selected = selected_pane_id == Some(row.pane_id.as_str());
        }
    }
}

fn visible_pane_ids<'a>(
    workspace_groups: &'a [WorkspaceGroup],
) -> impl Iterator<Item = &'a str> + 'a {
    workspace_groups.iter().flat_map(|group| group.rows.iter().map(|row| row.pane_id.as_str()))
}

fn contains_visible_pane_id(workspace_groups: &[WorkspaceGroup], pane_id: &str) -> bool {
    visible_pane_ids(workspace_groups).any(|candidate| candidate == pane_id)
}

fn first_visible_pane_id(workspace_groups: &[WorkspaceGroup]) -> Option<&str> {
    visible_pane_ids(workspace_groups).next()
}

fn apply_jump_matches(workspace_groups: &mut [WorkspaceGroup], pane_jump_digits: Option<&str>) {
    let digits = pane_jump_digits.filter(|digits| !digits.is_empty());
    for group in workspace_groups {
        for row in &mut group.rows {
            row.is_jump_match = digits
                .is_some_and(|digits| row.pane_id.trim_start_matches('%').starts_with(digits));
        }
    }
}

fn display_utc_offset() -> UtcOffset {
    UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
}

fn inactive_since_label(
    session: &SessionRecord,
    refreshed_at: Instant,
    refreshed_at_wallclock: SystemTime,
    display_offset: UtcOffset,
) -> String {
    if !matches!(
        session.status,
        SessionStatus::WaitingInput | SessionStatus::Finished | SessionStatus::Terminated
    ) {
        return String::new();
    }

    let elapsed = refreshed_at.saturating_duration_since(session.last_changed_at);
    let inactive_since =
        refreshed_at_wallclock.checked_sub(elapsed).unwrap_or(refreshed_at_wallclock);
    format_wallclock_hh_mm(inactive_since, display_offset)
}

fn format_wallclock_hh_mm(system_time: SystemTime, display_offset: UtcOffset) -> String {
    let local = OffsetDateTime::from(system_time).to_offset(display_offset);
    format!("{:02}:{:02}", local.hour(), local.minute())
}

fn normalize_expanded_pane_ids(
    expanded_pane_ids: &mut HashSet<String>,
    sessions: &[SessionRecord],
) {
    expanded_pane_ids.retain(|pane_id| {
        sessions.iter().any(|session| {
            session.pane.pane_id == *pane_id
                && session
                    .process_usage
                    .as_ref()
                    .is_some_and(|process_usage| !process_usage.subtasks.is_empty())
        })
    });
}

fn derive_path_labels<'a>(paths: impl IntoIterator<Item = &'a Path>) -> HashMap<PathBuf, String> {
    let mut unique_paths = Vec::new();
    let mut seen_paths = HashSet::new();
    for path in paths {
        let owned = path.to_path_buf();
        if seen_paths.insert(owned.clone()) {
            unique_paths.push(owned);
        }
    }

    let components: Vec<Vec<String>> =
        unique_paths.iter().map(|path| path_components(path)).collect();
    let mut labels = HashMap::new();
    let max_depth = components.iter().map(Vec::len).max().unwrap_or(1);

    for depth in 1..=max_depth {
        let mut candidates: HashMap<String, Vec<usize>> = HashMap::new();

        for (index, component_parts) in components.iter().enumerate() {
            if labels.contains_key(&unique_paths[index]) {
                continue;
            }

            candidates.entry(last_segments(component_parts, depth)).or_default().push(index);
        }

        for (label, indexes) in candidates {
            if indexes.len() == 1 {
                labels.insert(unique_paths[indexes[0]].clone(), label);
            }
        }
    }

    for path in unique_paths {
        let fallback = path.display().to_string();
        labels.entry(path).or_insert(fallback);
    }

    labels
}

fn path_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(ToOwned::to_owned)
        .collect()
}

fn last_segments(components: &[String], depth: usize) -> String {
    let start = components.len().saturating_sub(depth);
    components[start..].join("/")
}

fn status_line(git_report: &GitSummaryReport, process_warning: Option<&str>) -> String {
    let mut parts = Vec::new();

    if !git_report.warnings.is_empty() {
        let mut line = "git: ".to_string();
        if git_report.warnings.len() == 1 {
            line.push_str("1 workspace failed");
        } else {
            line.push_str(&format!("{} workspaces failed", git_report.warnings.len()));
        }
        parts.push(line);
    }

    if let Some(process_warning) = process_warning {
        parts.push(process_warning.to_string());
    }

    parts.join(" | ")
}

fn current_statuses(sessions: &[SessionRecord]) -> HashMap<String, SessionStatus> {
    sessions.iter().map(|session| (session.pane.pane_id.clone(), session.status)).collect()
}

fn count_alert_transitions(
    previous_statuses: &HashMap<String, SessionStatus>,
    current_sessions: &[SessionRecord],
) -> usize {
    current_sessions
        .iter()
        .filter(|session| {
            previous_statuses
                .get(&session.pane.pane_id)
                .is_some_and(|previous| should_alert_transition(*previous, session.status))
        })
        .count()
}

fn should_alert_transition(previous: SessionStatus, current: SessionStatus) -> bool {
    matches!(
        (previous, current),
        (SessionStatus::Running, SessionStatus::WaitingInput)
            | (SessionStatus::Running, SessionStatus::Finished)
            | (SessionStatus::Unknown, SessionStatus::WaitingInput)
            | (SessionStatus::Unknown, SessionStatus::Finished)
    )
}

fn refresh_interval_from_env() -> Duration {
    refresh_interval_from_var(env::var("ILMARI_REFRESH_SECONDS").ok().as_deref())
}

fn process_refresh_interval_from_env() -> Duration {
    process_refresh_interval_from_var(env::var("ILMARI_PROCESS_REFRESH_SECONDS").ok().as_deref())
}

fn quit_on_activate_from_env() -> bool {
    quit_on_activate_from_vars(
        env::var("TMUX").ok().as_deref(),
        env::var("TMUX_PANE").ok().as_deref(),
    )
}

fn quit_on_activate_from_vars(tmux: Option<&str>, tmux_pane: Option<&str>) -> bool {
    tmux.is_some() && tmux_pane.is_none()
}

fn refresh_interval_from_var(value: Option<&str>) -> Duration {
    refresh_interval_from_var_or(value, DEFAULT_REFRESH_INTERVAL)
}

fn process_refresh_interval_from_var(value: Option<&str>) -> Duration {
    refresh_interval_from_var_or(value, DEFAULT_PROCESS_REFRESH_INTERVAL)
}

fn refresh_interval_from_var_or(value: Option<&str>, default: Duration) -> Duration {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return default;
    };

    value
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(default)
}

fn available_render_width(frame: &Frame) -> usize {
    frame.area().width.saturating_sub(2) as usize
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

#[allow(dead_code)]
fn setup_terminal_manually(stdout: &mut Stdout) -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_model, count_alert_transitions, derive_path_labels, normalize_selected_pane_id,
        process_refresh_interval_from_var, quit_on_activate_from_vars, refresh_interval_from_var,
        relabel_git_summaries, App, CachedProcessTree, DEFAULT_PROCESS_REFRESH_INTERVAL,
        DEFAULT_REFRESH_INTERVAL,
    };
    use crate::colors::Palette;
    use crate::git::GitSummaryReport;
    use crate::model::{
        AgentDetail, AgentDetailTone, GitSummaryRow, ResourceUsage, SessionProcessUsage,
        SessionRecord, SessionStatus, SubtaskProcess,
    };
    use crate::process::{ProcessSnapshot, ProcessTree};
    use crate::tmux::PaneSnapshot;
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant, SystemTime};
    use time::UtcOffset;

    #[test]
    fn quit_keys_mark_the_app_for_exit() {
        let mut app = App::default();

        app.handle_key_event(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.should_quit);

        let mut app = App::default();
        app.handle_key_event(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.should_quit);
    }

    #[test]
    fn escape_does_not_mark_the_app_for_exit() {
        let mut app = App::default();

        app.handle_key_event(KeyCode::Esc, KeyModifiers::NONE);

        assert!(!app.should_quit);
    }

    #[test]
    fn movement_keys_do_not_crash_without_sessions() {
        let mut app = App::default();

        app.handle_key_event(KeyCode::Char('%'), KeyModifiers::NONE);
        app.handle_key_event(KeyCode::Char('1'), KeyModifiers::NONE);
        app.handle_key_event(KeyCode::Char('j'), KeyModifiers::NONE);
        app.handle_key_event(KeyCode::Down, KeyModifiers::NONE);
        app.handle_key_event(KeyCode::Char('k'), KeyModifiers::NONE);
        app.handle_key_event(KeyCode::Up, KeyModifiers::NONE);
        app.handle_key_event(KeyCode::Enter, KeyModifiers::NONE);

        assert!(app.sessions.is_empty());
        assert!(app.selected_pane_id.is_none());
    }

    #[test]
    fn movement_follows_visible_row_order() {
        let now = Instant::now();
        let sessions = vec![
            session_with_path("%7", SessionStatus::Running, "/tmp/zeta/blog"),
            session_with_path("%3", SessionStatus::WaitingInput, "/tmp/alpha/api"),
        ];
        let mut app = App::new(Palette::default(), DEFAULT_REFRESH_INTERVAL, false);
        app.sessions = sessions;
        app.selected_pane_id = Some("%3".to_string());
        app.sync_model("status".to_string(), Vec::new(), now, SystemTime::now());

        app.handle_key_event(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.selected_pane_id.as_deref(), Some("%7"));
    }

    #[test]
    fn sync_model_selects_most_recent_waiting_session_by_default() {
        let now = Instant::now();
        let mut app = App::new(Palette::default(), DEFAULT_REFRESH_INTERVAL, false);
        app.sessions = vec![
            session_with_change("%3", SessionStatus::WaitingInput, now - Duration::from_secs(60)),
            session_with_change("%7", SessionStatus::Running, now),
            session_with_change("%9", SessionStatus::WaitingInput, now),
        ];

        app.sync_model("status".to_string(), Vec::new(), now, SystemTime::now());

        assert_eq!(app.selected_pane_id.as_deref(), Some("%9"));
    }

    #[test]
    fn pane_jump_selects_exact_pane_id_match() {
        let now = Instant::now();
        let sessions = vec![
            session_with_path("%12", SessionStatus::Running, "/tmp/alpha/api"),
            session_with_path("%19", SessionStatus::WaitingInput, "/tmp/alpha/api"),
        ];
        let mut app = App::new(Palette::default(), DEFAULT_REFRESH_INTERVAL, false);
        app.sessions = sessions;
        app.selected_pane_id = Some("%12".to_string());
        app.sync_model("status".to_string(), Vec::new(), now, SystemTime::now());

        app.handle_key_event(KeyCode::Char('%'), KeyModifiers::NONE);
        app.handle_key_event(KeyCode::Char('1'), KeyModifiers::NONE);
        assert_eq!(app.selected_pane_id.as_deref(), Some("%12"));

        app.handle_key_event(KeyCode::Char('9'), KeyModifiers::NONE);
        assert_eq!(app.selected_pane_id.as_deref(), Some("%19"));
    }

    #[test]
    fn pane_jump_exits_on_non_digit_and_allows_normal_key_handling() {
        let now = Instant::now();
        let sessions = vec![
            session_with_path("%12", SessionStatus::Running, "/tmp/alpha/api"),
            session_with_path("%19", SessionStatus::WaitingInput, "/tmp/alpha/api"),
        ];
        let mut app = App::new(Palette::default(), DEFAULT_REFRESH_INTERVAL, false);
        app.sessions = sessions;
        app.selected_pane_id = Some("%12".to_string());
        app.sync_model("status".to_string(), Vec::new(), now, SystemTime::now());

        app.handle_key_event(KeyCode::Char('%'), KeyModifiers::NONE);
        app.handle_key_event(KeyCode::Char('1'), KeyModifiers::NONE);
        app.handle_key_event(KeyCode::Char('9'), KeyModifiers::NONE);
        assert_eq!(app.selected_pane_id.as_deref(), Some("%19"));

        app.handle_key_event(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.selected_pane_id.as_deref(), Some("%12"));
    }

    #[test]
    fn derive_path_labels_expands_colliding_suffixes() {
        let paths = [
            PathBuf::from("/tmp/api/worktree"),
            PathBuf::from("/srv/blog/worktree"),
            PathBuf::from("/tmp/shop/store"),
        ];
        let labels = derive_path_labels(paths.iter().map(PathBuf::as_path));

        assert_eq!(labels.get(Path::new("/tmp/api/worktree")), Some(&"api/worktree".to_string()));
        assert_eq!(labels.get(Path::new("/srv/blog/worktree")), Some(&"blog/worktree".to_string()));
        assert_eq!(labels.get(Path::new("/tmp/shop/store")), Some(&"store".to_string()));
    }

    #[test]
    fn build_model_relables_git_rows_from_paths() {
        let git_summaries = relabel_git_summaries(vec![
            GitSummaryRow {
                workspace_path: PathBuf::from("/tmp/api/worktree"),
                workspace_label: "worktree".to_string(),
                branch_name: "main".to_string(),
                insertions: 3,
                deletions: 1,
            },
            GitSummaryRow {
                workspace_path: PathBuf::from("/srv/blog/worktree"),
                workspace_label: "worktree".to_string(),
                branch_name: "feature".to_string(),
                insertions: 0,
                deletions: 0,
            },
        ]);

        assert_eq!(git_summaries[0].workspace_label, "api/worktree");
        assert_eq!(git_summaries[1].workspace_label, "blog/worktree");
    }

    #[test]
    fn build_model_includes_detected_client_in_workspace_rows() {
        let now = Instant::now();
        let sessions = vec![
            SessionRecord {
                pane: PaneSnapshot::parse(
                    "%3\t101\t$1\tdev\t@7\tagents\t0\t/Users/bnomei/Sites/ilmari\tcodex\ttitle",
                )
                .expect("pane snapshot should parse"),
                kind: crate::model::AgentKind::Codex,
                status: SessionStatus::Running,
                detail: Some(
                    AgentDetail {
                        label: "gpt-5.4 xhigh fast".to_string(),
                        tone: AgentDetailTone::Neutral,
                    }
                    .into(),
                ),
                output_excerpt: Some("Summarize the latest tracker behavior".to_string().into()),
                process_usage: Some(
                    SessionProcessUsage {
                        agent: ResourceUsage { cpu_tenths_percent: 154, memory_kib: 64 * 1024 },
                        spawned: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 12 * 1024 },
                        subtasks: vec![SubtaskProcess {
                            pid: 201,
                            depth: 0,
                            command_label: "tmux-mcp-rs".to_string(),
                            usage: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 12 * 1024 },
                        }],
                    }
                    .into(),
                ),
                output_fingerprint: None,
                last_changed_at: now,
                last_seen_at: now,
                retained_until: None,
            },
            SessionRecord {
                pane: PaneSnapshot::parse(
                    "%4\t102\t$1\tdev\t@7\tagents\t0\t/Users/bnomei/Sites/ilmari\tamp\ttitle",
                )
                .expect("pane snapshot should parse"),
                kind: crate::model::AgentKind::Amp,
                status: SessionStatus::WaitingInput,
                detail: Some(
                    AgentDetail { label: "smart".to_string(), tone: AgentDetailTone::Positive }
                        .into(),
                ),
                output_excerpt: Some(
                    "Could you clarify what you'd like me to help with?".to_string().into(),
                ),
                process_usage: Some(
                    SessionProcessUsage {
                        agent: ResourceUsage { cpu_tenths_percent: 21, memory_kib: 48 * 1024 },
                        spawned: ResourceUsage { cpu_tenths_percent: 0, memory_kib: 3 * 1024 },
                        subtasks: Vec::new(),
                    }
                    .into(),
                ),
                output_fingerprint: None,
                last_changed_at: now,
                last_seen_at: now,
                retained_until: None,
            },
        ];

        let model = build_model(
            &sessions,
            Vec::new(),
            Some("%4"),
            &HashSet::new(),
            None,
            "status".to_string(),
            Duration::from_secs(5),
            now,
            SystemTime::UNIX_EPOCH + Duration::from_secs(17 * 3600 + 42 * 60),
        );

        assert_eq!(model.workspace_groups.len(), 1);
        assert_eq!(model.workspace_groups[0].rows.len(), 2);
        assert_eq!(model.workspace_groups[0].rows[0].inactive_since_label, "");
        assert_eq!(model.workspace_groups[0].rows[0].client_label, "Codex");
        assert_eq!(
            model.workspace_groups[0].rows[0].detail,
            Some(
                AgentDetail {
                    label: "gpt-5.4 xhigh fast".to_string(),
                    tone: AgentDetailTone::Neutral,
                }
                .into()
            )
        );
        assert_eq!(model.workspace_groups[0].rows[0].pane_id, "%3");
        assert!(!model.workspace_groups[0].rows[0].is_jump_match);
        assert_eq!(model.workspace_groups[0].rows[0].status_label, "running");
        assert_eq!(model.workspace_groups[0].rows[0].status, SessionStatus::Running);
        assert_eq!(
            model.workspace_groups[0].rows[0].process_usage,
            Some(
                SessionProcessUsage {
                    agent: ResourceUsage { cpu_tenths_percent: 154, memory_kib: 64 * 1024 },
                    spawned: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 12 * 1024 },
                    subtasks: vec![SubtaskProcess {
                        pid: 201,
                        depth: 0,
                        command_label: "tmux-mcp-rs".to_string(),
                        usage: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 12 * 1024 },
                    }],
                }
                .into()
            )
        );
        assert!(!model.workspace_groups[0].rows[0].subtasks_expanded);
        assert_eq!(model.workspace_groups[0].rows[1].client_label, "Amp");
        assert_eq!(
            model.workspace_groups[0].rows[1].detail,
            Some(
                AgentDetail { label: "smart".to_string(), tone: AgentDetailTone::Positive }.into(),
            )
        );
        assert_eq!(
            model.workspace_groups[0].rows[1].output_excerpt.as_deref(),
            Some("Could you clarify what you'd like me to help with?")
        );
        assert_eq!(model.workspace_groups[0].rows[1].pane_id, "%4");
        assert_eq!(model.workspace_groups[0].rows[1].inactive_since_label.len(), 5);
        assert!(model.workspace_groups[0].rows[1].inactive_since_label.contains(':'));
        assert!(!model.workspace_groups[0].rows[1].is_jump_match);
        assert_eq!(model.workspace_groups[0].rows[1].status_label, "waiting-input");
        assert!(model.workspace_groups[0].rows[1].is_selected);
        assert_eq!(
            model.workspace_groups[0].rows[1].process_usage,
            Some(
                SessionProcessUsage {
                    agent: ResourceUsage { cpu_tenths_percent: 21, memory_kib: 48 * 1024 },
                    spawned: ResourceUsage { cpu_tenths_percent: 0, memory_kib: 3 * 1024 },
                    subtasks: Vec::new(),
                }
                .into()
            )
        );
    }

    #[test]
    fn build_model_marks_prefix_matches_during_pane_jump() {
        let now = Instant::now();
        let sessions = vec![
            session_with_path("%12", SessionStatus::Running, "/tmp/alpha/api"),
            session_with_path("%19", SessionStatus::WaitingInput, "/tmp/alpha/api"),
            session_with_path("%27", SessionStatus::Running, "/tmp/alpha/api"),
        ];

        let model = build_model(
            &sessions,
            Vec::new(),
            Some("%12"),
            &HashSet::new(),
            Some("1"),
            "status".to_string(),
            Duration::from_secs(5),
            now,
            SystemTime::now(),
        );

        let rows = &model.workspace_groups[0].rows;
        assert!(!rows[0].is_jump_match);
        assert!(rows[1].is_jump_match);
        assert!(rows[2].is_jump_match);
    }

    #[test]
    fn build_model_sorts_running_then_waiting_by_recency_then_remaining_descending() {
        let now = Instant::now();
        let sessions = vec![
            session_with_change("%4", SessionStatus::Finished, now - Duration::from_secs(40)),
            session_with_change("%12", SessionStatus::WaitingInput, now - Duration::from_secs(20)),
            session_with_change("%3", SessionStatus::Terminated, now - Duration::from_secs(10)),
            session_with_change("%18", SessionStatus::Running, now - Duration::from_secs(5)),
            session_with_change("%15", SessionStatus::WaitingInput, now - Duration::from_secs(2)),
            session_with_change("%9", SessionStatus::Unknown, now - Duration::from_secs(1)),
        ];

        let model = build_model(
            &sessions,
            Vec::new(),
            Some("%18"),
            &HashSet::new(),
            None,
            "status".to_string(),
            Duration::from_secs(5),
            now,
            SystemTime::now(),
        );

        assert_eq!(
            model.workspace_groups[0]
                .rows
                .iter()
                .map(|row| row.pane_id.as_str())
                .collect::<Vec<_>>(),
            vec!["%18", "%15", "%12", "%9", "%4", "%3"]
        );
    }

    #[test]
    fn equals_toggles_selected_subtasks() {
        let now = Instant::now();
        let mut app = App::new(Palette::default(), DEFAULT_REFRESH_INTERVAL, false);
        app.sessions = vec![SessionRecord {
            pane: PaneSnapshot::parse(
                "%12\t101\t$1\tdev\t@7\tagents\t0\t/Users/bnomei/Sites/ilmari\tcodex\ttitle",
            )
            .expect("pane snapshot should parse"),
            kind: crate::model::AgentKind::Codex,
            status: SessionStatus::Running,
            detail: None,
            output_excerpt: None,
            process_usage: Some(
                SessionProcessUsage {
                    agent: ResourceUsage { cpu_tenths_percent: 154, memory_kib: 64 * 1024 },
                    spawned: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 12 * 1024 },
                    subtasks: vec![SubtaskProcess {
                        pid: 201,
                        depth: 0,
                        command_label: "tmux-mcp-rs".to_string(),
                        usage: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 12 * 1024 },
                    }],
                }
                .into(),
            ),
            output_fingerprint: None,
            last_changed_at: now,
            last_seen_at: now,
            retained_until: None,
        }];
        app.selected_pane_id = Some("%12".to_string());
        app.sync_model("status".to_string(), Vec::new(), now, SystemTime::now());

        assert!(!app.model.workspace_groups[0].rows[0].subtasks_expanded);

        app.handle_key_event(KeyCode::Char('='), KeyModifiers::NONE);
        assert!(app.expanded_pane_ids.contains("%12"));
        assert!(app.model.workspace_groups[0].rows[0].subtasks_expanded);

        app.handle_key_event(KeyCode::Char('='), KeyModifiers::NONE);
        assert!(!app.expanded_pane_ids.contains("%12"));
        assert!(!app.model.workspace_groups[0].rows[0].subtasks_expanded);
    }

    #[test]
    fn equals_lazily_hydrates_selected_process_usage() {
        let now = Instant::now();
        let mut app = App::new(Palette::default(), DEFAULT_REFRESH_INTERVAL, false);
        app.sessions = vec![session("%12", SessionStatus::Running)];
        app.process_cache.cached_tree = Some(CachedProcessTree::new(sample_process_tree(), now));
        app.selected_pane_id = Some("%12".to_string());
        app.sync_model("status".to_string(), Vec::new(), now, SystemTime::now());

        assert!(app.sessions[0].process_usage.is_none());
        assert!(app.model.workspace_groups[0].rows[0].process_usage.is_none());

        app.handle_key_event(KeyCode::Char('='), KeyModifiers::NONE);

        assert!(app.expanded_pane_ids.contains("%12"));
        assert!(app.sessions[0].process_usage.is_some());
        assert!(app.model.workspace_groups[0].rows[0].process_usage.is_some());
        assert!(app.model.workspace_groups[0].rows[0].subtasks_expanded);
    }

    #[test]
    fn app_visibility_toggles_with_a_and_defaults_to_disabled() {
        let mut app = App::default();

        assert!(!app.model.show_app);

        app.handle_key_event(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(app.model.show_app);

        app.handle_key_event(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(!app.model.show_app);
    }

    #[test]
    fn bell_toggle_defaults_on_and_toggles_with_b() {
        let mut app = App::default();

        assert!(app.bell_enabled);

        app.handle_key_event(KeyCode::Char('b'), KeyModifiers::NONE);
        assert!(!app.bell_enabled);
        assert_eq!(app.model.status_line, "bell: off");

        app.handle_key_event(KeyCode::Char('b'), KeyModifiers::NONE);
        assert!(app.bell_enabled);
        assert_eq!(app.model.status_line, "bell: on");
    }

    #[test]
    fn model_visibility_toggles_with_m_and_defaults_to_disabled() {
        let mut app = App::default();

        assert!(!app.model.show_detail);

        app.handle_key_event(KeyCode::Char('m'), KeyModifiers::NONE);
        assert!(app.model.show_detail);

        app.handle_key_event(KeyCode::Char('m'), KeyModifiers::NONE);
        assert!(!app.model.show_detail);
    }

    #[test]
    fn git_visibility_toggles_with_g_and_defaults_to_enabled() {
        let mut app = App::default();

        assert!(app.model.show_git);

        app.handle_key_event(KeyCode::Char('g'), KeyModifiers::NONE);
        assert!(!app.model.show_git);

        app.handle_key_event(KeyCode::Char('g'), KeyModifiers::NONE);
        assert!(app.model.show_git);
    }

    #[test]
    fn hiding_git_releases_cached_git_rows() {
        let now = Instant::now();
        let mut app = App::new(Palette::default(), DEFAULT_REFRESH_INTERVAL, false);
        app.sessions = vec![session_with_path("%12", SessionStatus::Running, "/tmp/api/worktree")];
        app.sync_model(
            "status".to_string(),
            vec![GitSummaryRow {
                workspace_path: PathBuf::from("/tmp/api/worktree"),
                workspace_label: "worktree".to_string(),
                branch_name: "main".to_string(),
                insertions: 3,
                deletions: 1,
            }],
            now,
            SystemTime::now(),
        );

        assert!(!app.git_summaries.is_empty());
        assert!(app.model.workspace_groups[0].git_summary.is_some());

        app.handle_key_event(KeyCode::Char('g'), KeyModifiers::NONE);

        assert!(!app.model.show_git);
        assert!(app.git_summaries.is_empty());
        assert!(app.model.workspace_groups[0].git_summary.is_none());
    }

    #[test]
    fn stats_visibility_toggles_with_s_and_defaults_to_disabled() {
        let mut app = App::default();

        assert!(!app.model.show_stats);

        app.handle_key_event(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(app.model.show_stats);

        app.handle_key_event(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(!app.model.show_stats);
    }

    #[test]
    fn stats_toggle_hydrates_process_usage_from_cached_tree() {
        let now = Instant::now();
        let mut app = App::new(Palette::default(), DEFAULT_REFRESH_INTERVAL, false);
        app.sessions = vec![session("%12", SessionStatus::Running)];
        app.process_cache.cached_tree = Some(CachedProcessTree::new(sample_process_tree(), now));
        app.selected_pane_id = Some("%12".to_string());
        app.sync_model("status".to_string(), Vec::new(), now, SystemTime::now());

        assert!(!app.model.show_stats);
        assert!(app.sessions[0].process_usage.is_none());

        app.handle_key_event(KeyCode::Char('s'), KeyModifiers::NONE);

        assert!(app.model.show_stats);
        assert!(app.sessions[0].process_usage.is_some());
        assert!(app.model.workspace_groups[0].rows[0].process_usage.is_some());
    }

    #[test]
    fn hiding_stats_releases_process_usage_when_no_subtasks_are_expanded() {
        let now = Instant::now();
        let mut app = App::new(Palette::default(), DEFAULT_REFRESH_INTERVAL, false);
        app.sessions = vec![session("%12", SessionStatus::Running)];
        app.process_cache.cached_tree = Some(CachedProcessTree::new(sample_process_tree(), now));
        app.selected_pane_id = Some("%12".to_string());
        app.sync_model("status".to_string(), Vec::new(), now, SystemTime::now());

        app.handle_key_event(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(app.sessions[0].process_usage.is_some());
        assert!(app.process_cache.cached_tree.is_some());

        app.handle_key_event(KeyCode::Char('s'), KeyModifiers::NONE);

        assert!(!app.model.show_stats);
        assert!(app.sessions[0].process_usage.is_none());
        assert!(app.process_cache.cached_tree.is_none());
    }

    #[test]
    fn time_visibility_toggles_with_t_and_defaults_to_enabled() {
        let mut app = App::default();

        assert!(app.model.show_time);

        app.handle_key_event(KeyCode::Char('t'), KeyModifiers::NONE);
        assert!(!app.model.show_time);

        app.handle_key_event(KeyCode::Char('t'), KeyModifiers::NONE);
        assert!(app.model.show_time);
    }

    #[test]
    fn output_visibility_toggles_with_o_and_defaults_to_enabled() {
        let mut app = App::default();

        assert!(app.model.show_output);

        app.handle_key_event(KeyCode::Char('o'), KeyModifiers::NONE);
        assert!(!app.model.show_output);

        app.handle_key_event(KeyCode::Char('o'), KeyModifiers::NONE);
        assert!(app.model.show_output);
    }

    #[test]
    fn responsive_defaults_keep_extra_columns_hidden_when_width_is_wide() {
        let mut app = App::default();

        app.apply_responsive_view_defaults(81);

        assert!(!app.model.show_app);
        assert!(!app.model.show_detail);
        assert!(app.model.show_time);
        assert!(app.model.show_output);
        assert!(!app.model.show_stats);
        assert!(app.model.show_git);
    }

    #[test]
    fn responsive_defaults_keep_hidden_columns_disabled_when_width_is_narrow() {
        let mut app = App::default();

        app.apply_responsive_view_defaults(80);

        assert!(!app.model.show_app);
        assert!(!app.model.show_detail);
        assert!(app.model.show_time);
        assert!(app.model.show_output);
        assert!(!app.model.show_stats);
        assert!(app.model.show_git);
    }

    #[test]
    fn manual_toggles_pin_visibility_against_responsive_defaults() {
        let mut app = App::default();

        app.handle_key_event(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(app.model.show_app);

        app.handle_key_event(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(!app.model.show_app);

        app.apply_responsive_view_defaults(120);
        assert!(!app.model.show_app);

        app.handle_key_event(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(app.model.show_app);

        app.apply_responsive_view_defaults(120);
        assert!(app.model.show_app);

        app.apply_responsive_view_defaults(60);
        assert!(app.model.show_app);
        assert!(!app.model.show_detail);
        assert!(app.model.show_time);
        assert!(app.model.show_output);
        assert!(!app.model.show_stats);
    }

    #[test]
    fn refresh_interval_parser_accepts_positive_second_values() {
        assert_eq!(refresh_interval_from_var(Some("12")), Duration::from_secs(12));
    }

    #[test]
    fn refresh_interval_parser_falls_back_for_invalid_values() {
        assert_eq!(refresh_interval_from_var(Some("0")), DEFAULT_REFRESH_INTERVAL);
        assert_eq!(refresh_interval_from_var(Some("not-a-number")), DEFAULT_REFRESH_INTERVAL);
        assert_eq!(refresh_interval_from_var(Some("  ")), DEFAULT_REFRESH_INTERVAL);
    }

    #[test]
    fn process_refresh_interval_parser_uses_process_default() {
        assert_eq!(process_refresh_interval_from_var(Some("12")), Duration::from_secs(12));
        assert_eq!(process_refresh_interval_from_var(Some("0")), DEFAULT_PROCESS_REFRESH_INTERVAL);
        assert_eq!(
            process_refresh_interval_from_var(Some("not-a-number")),
            DEFAULT_PROCESS_REFRESH_INTERVAL
        );
    }

    #[test]
    fn popup_launch_context_quits_on_activate() {
        assert!(quit_on_activate_from_vars(Some("/tmp/tmux"), None));
        assert!(!quit_on_activate_from_vars(Some("/tmp/tmux"), Some("%1")));
        assert!(!quit_on_activate_from_vars(None, None));
    }

    #[test]
    fn normalize_selected_pane_id_prefers_most_recent_waiting_row() {
        let now = Instant::now();
        let sessions = vec![
            session_with_change("%2", SessionStatus::WaitingInput, now - Duration::from_secs(60)),
            session_with_change("%8", SessionStatus::WaitingInput, now),
        ];
        let model = build_model(
            &sessions,
            Vec::new(),
            None,
            &HashSet::new(),
            None,
            String::new(),
            Duration::from_secs(5),
            now,
            SystemTime::now(),
        );
        let normalized = normalize_selected_pane_id(None, &model.workspace_groups, &sessions);

        assert_eq!(normalized.as_deref(), Some("%8"));
    }

    #[test]
    fn normalize_selected_pane_id_defaults_to_first_visible_row() {
        let sessions =
            vec![session("%2", SessionStatus::Running), session("%8", SessionStatus::Running)];
        let model = build_model(
            &sessions,
            Vec::new(),
            None,
            &HashSet::new(),
            None,
            String::new(),
            Duration::from_secs(5),
            Instant::now(),
            SystemTime::now(),
        );
        let normalized = normalize_selected_pane_id(None, &model.workspace_groups, &sessions);
        assert_eq!(normalized.as_deref(), Some("%8"));
    }

    #[test]
    fn inactive_since_label_formats_hh_mm_for_waiting_session() {
        let refreshed_at = Instant::now();
        let label = super::inactive_since_label(
            &session_with_change("%2", SessionStatus::WaitingInput, refreshed_at),
            refreshed_at,
            SystemTime::UNIX_EPOCH + Duration::from_secs(17 * 3600 + 42 * 60),
            UtcOffset::UTC,
        );

        assert_eq!(label, "17:42");
    }

    #[test]
    fn transition_counter_ignores_non_actionable_state_changes() {
        let previous = HashMap::from([
            ("%1".to_string(), SessionStatus::Finished),
            ("%2".to_string(), SessionStatus::WaitingInput),
        ]);
        let current =
            vec![session("%1", SessionStatus::Terminated), session("%2", SessionStatus::Running)];

        assert_eq!(count_alert_transitions(&previous, &current), 0);
    }

    #[test]
    fn transition_counter_counts_only_meaningful_alerts() {
        let previous = HashMap::from([
            ("%1".to_string(), SessionStatus::Running),
            ("%2".to_string(), SessionStatus::Unknown),
            ("%3".to_string(), SessionStatus::WaitingInput),
        ]);
        let current = vec![
            session("%1", SessionStatus::Finished),
            session("%2", SessionStatus::WaitingInput),
            session("%3", SessionStatus::Finished),
        ];

        assert_eq!(count_alert_transitions(&previous, &current), 2);
    }

    #[test]
    fn status_line_reports_git_warning_count() {
        let report = GitSummaryReport {
            rows: Vec::new(),
            warnings: vec!["boom".to_string(), "pow".to_string()],
        };

        assert_eq!(super::status_line(&report, None), "git: 2 workspaces failed");
    }

    #[test]
    fn status_line_is_empty_without_git_warnings() {
        let report = GitSummaryReport { rows: Vec::new(), warnings: Vec::new() };

        assert_eq!(super::status_line(&report, None), "");
    }

    #[test]
    fn status_line_combines_git_and_process_warnings() {
        let report = GitSummaryReport { rows: Vec::new(), warnings: vec!["boom".to_string()] };

        assert_eq!(
            super::status_line(&report, Some("ps: snapshot failed")),
            "git: 1 workspace failed | ps: snapshot failed"
        );
    }

    fn session(pane_id: &str, status: SessionStatus) -> SessionRecord {
        session_with_path(pane_id, status, "/Users/bnomei/Sites/ilmari")
    }

    fn session_with_change(
        pane_id: &str,
        status: SessionStatus,
        last_changed_at: Instant,
    ) -> SessionRecord {
        let mut session = session(pane_id, status);
        session.last_changed_at = last_changed_at;
        session
    }

    fn session_with_path(pane_id: &str, status: SessionStatus, path: &str) -> SessionRecord {
        let now = Instant::now();
        SessionRecord {
            pane: PaneSnapshot::parse(&format!(
                "{pane_id}\t101\t$1\tdev\t@7\tagents\t0\t{path}\tcodex\ttitle"
            ))
            .expect("pane snapshot should parse"),
            kind: crate::model::AgentKind::Codex,
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

    fn sample_process_tree() -> ProcessTree {
        ProcessTree::from_snapshots(vec![
            ProcessSnapshot {
                pid: 101,
                ppid: 1,
                cpu_tenths_percent: 0,
                memory_kib: 0,
                command: "zsh".to_string(),
            },
            ProcessSnapshot {
                pid: 201,
                ppid: 101,
                cpu_tenths_percent: 154,
                memory_kib: 64 * 1024,
                command: "codex".to_string(),
            },
            ProcessSnapshot {
                pid: 202,
                ppid: 201,
                cpu_tenths_percent: 8,
                memory_kib: 12 * 1024,
                command: "tmux-mcp-rs".to_string(),
            },
        ])
    }
}
