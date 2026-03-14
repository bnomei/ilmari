use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::model::{AgentDetail, AgentDetailTone, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

pub const DEFAULT_RETENTION: Duration = Duration::from_secs(30);
const STATUS_SIGNAL_WINDOW_BYTES: usize = 240;
const PROMPT_LINE_WINDOW: usize = 6;
const OUTPUT_EXCERPT_MAX_CHARS: usize = 80;

pub trait AgentAdapter {
    fn kind(&self) -> AgentKind;
    fn detect(&self, pane: &PaneSnapshot) -> bool;
    fn classify(
        &self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        output_fingerprint: Option<u64>,
        previous: Option<&SessionRecord>,
    ) -> SessionStatus;
    fn extract_detail(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<AgentDetail>>;
    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>>;
}

#[derive(Default)]
pub struct AdapterRegistry {
    adapters: Vec<Box<dyn AgentAdapter>>,
}

impl AdapterRegistry {
    pub fn v1() -> Self {
        Self {
            adapters: vec![
                Box::new(CodexAdapter),
                Box::new(AmpAdapter),
                Box::new(ClaudeCodeAdapter),
                Box::new(OpenCodeAdapter),
                Box::new(PiAdapter),
            ],
        }
    }

    #[cfg(test)]
    pub fn detect_kind(
        &self,
        pane: &PaneSnapshot,
        previous: Option<&SessionRecord>,
    ) -> Option<AgentKind> {
        self.select_adapter(pane, previous).map(AgentAdapter::kind)
    }

    pub fn needs_output_tail(&self, pane: &PaneSnapshot, previous: Option<&SessionRecord>) -> bool {
        !pane.pane_dead
            && !is_shell_command(&pane.pane_current_command)
            && self.select_adapter(pane, previous).is_some()
    }

    fn select_adapter<'a>(
        &'a self,
        pane: &PaneSnapshot,
        previous: Option<&SessionRecord>,
    ) -> Option<&'a dyn AgentAdapter> {
        if let Some(previous) = previous {
            if let Some(adapter) = self
                .adapters
                .iter()
                .find(|adapter| adapter.kind() == previous.kind)
                .map(Box::as_ref)
            {
                if pane.pane_dead
                    || adapter.detect(pane)
                    || is_shell_command(&pane.pane_current_command)
                {
                    return Some(adapter);
                }
            }
        }

        self.adapters.iter().find(|adapter| adapter.detect(pane)).map(Box::as_ref)
    }
}

pub struct SessionTracker {
    registry: AdapterRegistry,
    retention: Duration,
    records: HashMap<String, SessionRecord>,
}

impl Default for SessionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionTracker {
    pub fn new() -> Self {
        Self::with_retention(DEFAULT_RETENTION)
    }

    pub fn with_retention(retention: Duration) -> Self {
        Self { registry: AdapterRegistry::v1(), retention, records: HashMap::new() }
    }

    pub fn registry(&self) -> &AdapterRegistry {
        &self.registry
    }

    pub fn records(&self) -> &HashMap<String, SessionRecord> {
        &self.records
    }

    pub fn refresh(
        &mut self,
        panes: &[PaneSnapshot],
        output_tails: &HashMap<String, String>,
        now: Instant,
    ) -> Vec<SessionRecord> {
        let previous = std::mem::take(&mut self.records);
        let mut next = HashMap::new();
        let mut seen = HashSet::new();

        for pane in panes {
            seen.insert(pane.pane_id.clone());

            if let Some(record) = self.classify_pane(
                pane,
                output_tails.get(&pane.pane_id).map(String::as_str),
                previous.get(&pane.pane_id),
                now,
            ) {
                next.insert(record.pane.pane_id.clone(), record);
            }
        }

        for (pane_id, record) in &previous {
            if seen.contains(pane_id) {
                continue;
            }

            if let Some(retained) = self.retain_missing_record(record, now) {
                next.insert(pane_id.clone(), retained);
            }
        }

        self.records = next;

        let mut records: Vec<_> = self.records.values().cloned().collect();
        records.sort_by(|left, right| left.pane.pane_id.cmp(&right.pane.pane_id));
        records
    }

    fn classify_pane(
        &self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
        now: Instant,
    ) -> Option<SessionRecord> {
        let adapter = self.registry.select_adapter(pane, previous)?;
        let output_fingerprint = output_tail.and_then(full_output_fingerprint);
        let status = adapter.classify(pane, output_tail, output_fingerprint, previous);
        let detail = adapter.extract_detail(output_tail, previous);
        let output_excerpt = adapter.extract_output_excerpt(output_tail, previous);
        let retained_until = retention_deadline(previous, status, self.retention, now)?;
        let last_changed_at = match previous {
            Some(previous) if previous.kind == adapter.kind() && previous.status == status => {
                previous.last_changed_at
            }
            _ => now,
        };

        Some(SessionRecord {
            pane: pane.clone(),
            kind: adapter.kind(),
            status,
            detail,
            output_excerpt,
            process_usage: previous.and_then(|record| record.process_usage.clone()),
            output_fingerprint,
            last_changed_at,
            last_seen_at: now,
            retained_until,
        })
    }

    fn retain_missing_record(
        &self,
        previous: &SessionRecord,
        now: Instant,
    ) -> Option<SessionRecord> {
        if previous.status == SessionStatus::Terminated {
            return None;
        }

        Some(SessionRecord {
            pane: previous.pane.clone(),
            kind: previous.kind,
            status: SessionStatus::Terminated,
            detail: previous.detail.clone(),
            output_excerpt: previous.output_excerpt.clone(),
            process_usage: previous.process_usage.clone(),
            output_fingerprint: previous.output_fingerprint,
            last_changed_at: match previous.status {
                SessionStatus::Terminated => previous.last_changed_at,
                _ => now,
            },
            last_seen_at: previous.last_seen_at,
            retained_until: None,
        })
    }
}

struct CodexAdapter;

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

struct AmpAdapter;

impl AgentAdapter for AmpAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Amp
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "amp")
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
        reuse_detail_arc(extract_amp_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_amp_output_excerpt(output_tail), previous)
    }
}

struct ClaudeCodeAdapter;

impl AgentAdapter for ClaudeCodeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_equals_any(&pane.pane_current_command, &["claude", "claude-code"])
            || pane_title_contains(&pane.pane_title, "claude code")
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

struct OpenCodeAdapter;

impl AgentAdapter for OpenCodeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::OpenCode
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_matches(&pane.pane_current_command, "opencode")
            || pane_title_contains(&pane.pane_title, "oc |")
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
        reuse_detail_arc(extract_opencode_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_opencode_output_excerpt(output_tail), previous)
    }
}

struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Pi
    }

    fn detect(&self, pane: &PaneSnapshot) -> bool {
        command_equals_any(&pane.pane_current_command, &["pi", "pi-agent"])
            || pane.pane_title.contains('π')
            || pane_title_contains(&pane.pane_title, "pi v")
    }

    fn classify(
        &self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        output_fingerprint: Option<u64>,
        previous: Option<&SessionRecord>,
    ) -> SessionStatus {
        classify_pi_session(self, pane, output_tail, output_fingerprint, previous)
    }

    fn extract_detail(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<AgentDetail>> {
        reuse_detail_arc(extract_pi_detail(output_tail), previous)
    }

    fn extract_output_excerpt(
        &self,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
    ) -> Option<Arc<str>> {
        reuse_output_excerpt_arc(extract_pi_output_excerpt(output_tail), previous)
    }
}

fn classify_supported_session(
    adapter: &dyn AgentAdapter,
    pane: &PaneSnapshot,
    output_tail: Option<&str>,
    output_fingerprint: Option<u64>,
    previous: Option<&SessionRecord>,
) -> SessionStatus {
    if pane.pane_dead {
        return SessionStatus::Terminated;
    }

    if previous.is_some() && !adapter.detect(pane) && is_shell_command(&pane.pane_current_command) {
        return SessionStatus::Finished;
    }

    if let Some(retained_status) = retained_status_without_output_tail(output_tail, previous) {
        return retained_status;
    }

    let output_tail = output_tail.unwrap_or_default();

    if output_has_recent_motion(output_fingerprint, previous) {
        return SessionStatus::Running;
    }

    if let Some(status) = classify_output_tail(output_tail) {
        return status;
    }

    if output_is_stable(output_fingerprint, previous) {
        return SessionStatus::WaitingInput;
    }

    if adapter.detect(pane) {
        return SessionStatus::WaitingInput;
    }

    SessionStatus::Unknown
}

fn classify_pi_session(
    adapter: &dyn AgentAdapter,
    pane: &PaneSnapshot,
    output_tail: Option<&str>,
    output_fingerprint: Option<u64>,
    previous: Option<&SessionRecord>,
) -> SessionStatus {
    if pane.pane_dead {
        return SessionStatus::Terminated;
    }

    if previous.is_some() && !adapter.detect(pane) && is_shell_command(&pane.pane_current_command) {
        return SessionStatus::Finished;
    }

    if let Some(retained_status) = retained_status_without_output_tail(output_tail, previous) {
        return retained_status;
    }

    let output_tail = output_tail.unwrap_or_default();

    if output_has_recent_motion(output_fingerprint, previous) {
        return SessionStatus::Running;
    }

    if looks_like_pi_idle(output_tail) {
        return SessionStatus::WaitingInput;
    }

    if let Some(status) = classify_output_tail(output_tail) {
        return status;
    }

    if output_is_stable(output_fingerprint, previous) {
        return SessionStatus::WaitingInput;
    }

    if adapter.detect(pane) {
        return SessionStatus::WaitingInput;
    }

    SessionStatus::Unknown
}

fn retention_deadline(
    previous: Option<&SessionRecord>,
    status: SessionStatus,
    retention: Duration,
    now: Instant,
) -> Option<Option<Instant>> {
    if !status.uses_retention() {
        return Some(None);
    }

    match previous {
        Some(previous) if previous.status == status => match previous.retained_until {
            Some(until) if now <= until => Some(Some(until)),
            Some(_) => None,
            None => Some(Some(now + retention)),
        },
        _ => Some(Some(now + retention)),
    }
}

fn retained_status_without_output_tail(
    output_tail: Option<&str>,
    previous: Option<&SessionRecord>,
) -> Option<SessionStatus> {
    if output_tail.is_some() {
        return None;
    }

    previous
        .map(|previous| previous.status)
        .filter(|status| matches!(status, SessionStatus::WaitingInput | SessionStatus::Finished))
}

fn is_shell_command(command: &str) -> bool {
    let normalized = normalized_command_name(command);
    matches!(normalized.as_str(), "fish" | "nu") || normalized == "sh" || normalized.ends_with("sh")
}

fn command_matches(command: &str, expected: &str) -> bool {
    let normalized = normalized_command_name(command);
    normalized == expected || normalized.starts_with(&format!("{expected}-"))
}

fn command_equals_any(command: &str, expected: &[&str]) -> bool {
    let normalized = normalized_command_name(command);
    expected.iter().any(|candidate| normalized == *candidate)
}

fn pane_title_contains(title: &str, needle: &str) -> bool {
    title.to_ascii_lowercase().contains(needle)
}

fn normalized_command_name(command: &str) -> String {
    command.trim().rsplit('/').next().unwrap_or_default().to_ascii_lowercase()
}

fn classify_output_tail(output_tail: &str) -> Option<SessionStatus> {
    let recent_lines = recent_nonempty_lines(output_tail, PROMPT_LINE_WINDOW);
    if looks_like_waiting_prompt(&recent_lines, output_tail) {
        return Some(SessionStatus::WaitingInput);
    }

    latest_recent_match(output_tail, waiting_pattern()).map(|_| SessionStatus::WaitingInput)
}

fn extract_codex_detail(output_tail: Option<&str>) -> Option<AgentDetail> {
    let output_tail = output_tail?;
    let label = codex_card_model_pattern()
        .captures(output_tail)
        .and_then(|captures| captures.name("model"))
        .map(|matched| normalize_detail_label(matched.as_str()))
        .filter(|label| !label.is_empty())
        .or_else(|| {
            output_tail.lines().rev().find_map(|line| {
                codex_footer_model_pattern()
                    .captures(line.trim())
                    .and_then(|captures| captures.name("model"))
                    .map(|matched| normalize_detail_label(matched.as_str()))
                    .filter(|label| !label.is_empty())
            })
        })?;

    Some(AgentDetail { label, tone: AgentDetailTone::Neutral })
}

fn extract_amp_detail(output_tail: Option<&str>) -> Option<AgentDetail> {
    let output_tail = output_tail?;
    let label = output_tail.lines().rev().find_map(|line| {
        let trimmed = line.trim();
        if !trimmed.to_ascii_lowercase().contains("skills") {
            return None;
        }

        amp_mode_pattern()
            .captures(trimmed)
            .and_then(|captures| captures.name("mode"))
            .map(|matched| matched.as_str().to_ascii_lowercase())
    })?;

    let tone = match label.as_str() {
        "smart" => AgentDetailTone::Positive,
        "rush" => AgentDetailTone::Warning,
        _ => AgentDetailTone::Neutral,
    };

    Some(AgentDetail { label, tone })
}

fn extract_claude_detail(output_tail: Option<&str>) -> Option<AgentDetail> {
    let output_tail = output_tail?;
    let model = extract_claude_model_label(output_tail)
        .or_else(|| extract_named_model_detail(Some(output_tail)).map(|detail| detail.label));
    let effort = extract_claude_effort_label(output_tail);

    let label = match (model, effort) {
        (Some(model), Some(effort)) => format!("{model} {effort}"),
        (Some(model), None) => model,
        (None, Some(_)) => return None,
        (None, None) => return None,
    };

    Some(AgentDetail { label, tone: AgentDetailTone::Neutral })
}

fn extract_opencode_detail(output_tail: Option<&str>) -> Option<AgentDetail> {
    let output_tail = output_tail?;
    output_tail
        .lines()
        .rev()
        .find_map(extract_opencode_status_line)
        .or_else(|| output_tail.lines().rev().find_map(extract_opencode_build_line))
        .map(|label| AgentDetail { label, tone: AgentDetailTone::Neutral })
}

fn extract_pi_detail(output_tail: Option<&str>) -> Option<AgentDetail> {
    let output_tail = output_tail?;
    let model = extract_pi_footer_model(output_tail)
        .or_else(|| extract_named_model_detail(Some(output_tail)).map(|detail| detail.label));
    let effort = extract_pi_footer_effort(output_tail);

    let label = match (model, effort) {
        (Some(model), Some(effort)) => format!("{model} {effort}"),
        (Some(model), None) => model,
        (None, Some(_)) => return None,
        (None, None) => return None,
    };

    Some(AgentDetail { label, tone: AgentDetailTone::Neutral })
}

fn extract_codex_output_excerpt(output_tail: Option<&str>) -> Option<String> {
    let output_tail = output_tail?;
    extract_output_excerpt_from_tail(output_tail, |raw, normalized| {
        let lower = normalized.to_ascii_lowercase();
        is_common_output_noise(raw, normalized)
            || lower.contains("/model to change")
            || lower.contains("% left")
    })
}

fn extract_amp_output_excerpt(output_tail: Option<&str>) -> Option<String> {
    let output_tail = output_tail?;
    extract_output_excerpt_from_tail(output_tail, |raw, normalized| {
        let lower = normalized.to_ascii_lowercase();
        let raw_lower = raw.to_ascii_lowercase();
        is_common_output_noise(raw, normalized)
            || raw_lower.contains("welcome to")
            || ((lower.contains("smart") || lower.contains("rush")) && lower.contains("skills"))
            || (normalized.contains("~/") && normalized.contains('('))
            || raw.contains('✓') && lower.contains("thinking")
            || lower.contains("of 168k")
            || (normalized.chars().count() == 1 && raw.trim_start().starts_with(['│', '┃']))
    })
}

fn extract_claude_output_excerpt(output_tail: Option<&str>) -> Option<String> {
    let output_tail = output_tail?;
    extract_output_excerpt_from_tail(output_tail, |raw, normalized| {
        let lower = normalized.to_ascii_lowercase();
        let compact = lower.trim_start_matches(|c: char| !c.is_ascii_alphanumeric()).trim_start();
        is_common_output_noise(raw, normalized)
            || lower.contains("claude code")
            || lower.contains("? for shortcuts")
            || lower.contains("/effort")
            || lower.starts_with("select model")
            || lower.contains("enter to confirm")
            || lower.contains("choose the text style that looks best")
            || claude_elapsed_footer_pattern().is_match(compact)
            || normalized.starts_with('❯')
    })
}

fn extract_opencode_output_excerpt(output_tail: Option<&str>) -> Option<String> {
    let output_tail = output_tail?;
    extract_output_excerpt_from_tail(output_tail, |raw, normalized| {
        let lower = normalized.to_ascii_lowercase();
        is_common_output_noise(raw, normalized)
            || lower.contains("conversation title:")
            || lower.contains("tab agents")
            || lower.contains("ctrl+p commands")
            || raw.trim_start().starts_with('▣')
            || lower.contains("thinking:")
            || lower == "opencode"
            || lower.starts_with("ask anything")
            || (lower.starts_with("build ") && lower.contains("opencode zen"))
    })
}

fn extract_pi_output_excerpt(output_tail: Option<&str>) -> Option<String> {
    let output_tail = output_tail?;
    extract_output_excerpt_from_tail(output_tail, |raw, normalized| {
        let lower = normalized.to_ascii_lowercase();
        is_common_output_noise(raw, normalized)
            || lower.contains("pi assistant")
            || lower.starts_with("pi v")
            || lower.starts_with("model:")
            || lower.starts_with("session:")
            || lower.starts_with("tools:")
            || lower.starts_with("you:")
            || lower.starts_with("escape to ")
            || lower.starts_with("ctrl+")
            || lower.starts_with("shift+")
            || lower.starts_with("alt+")
            || lower.starts_with("/ for commands")
            || lower.starts_with("! to run bash")
            || lower.starts_with("!! to run bash")
            || lower.starts_with("drop files to attach")
            || lower.starts_with("[context]")
            || lower.starts_with("[skills]")
            || normalized.starts_with("~/")
            || normalized.starts_with("~/.")
            || normalized.starts_with("/Users/")
            || raw.contains('↑')
            || raw.contains('↓')
            || lower.contains("ctrl+l to select model")
            || lower.contains("warning: no models available")
            || lower.starts_with("warning:")
            || pi_footer_pattern().is_match(normalized)
    })
}

fn extract_output_excerpt_from_tail<F>(output_tail: &str, mut is_noise: F) -> Option<String>
where
    F: FnMut(&str, &str) -> bool,
{
    let mut blocks: Vec<Vec<(String, bool)>> = Vec::new();
    let mut current_block: Vec<(String, bool)> = Vec::new();

    for raw in output_tail.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            if !current_block.is_empty() {
                blocks.push(std::mem::take(&mut current_block));
            }
            continue;
        }

        let normalized = normalize_output_line(trimmed);
        if normalized.is_empty() || is_noise(trimmed, &normalized) {
            if !current_block.is_empty() {
                blocks.push(std::mem::take(&mut current_block));
            }
            continue;
        }

        let from_box = trimmed.starts_with(['│', '┃']);
        current_block.push((normalized, from_box));
    }

    if !current_block.is_empty() {
        blocks.push(current_block);
    }

    for block in blocks.into_iter().rev() {
        let pieces: Vec<String> = if block.iter().any(|(_, from_box)| !*from_box) {
            block.into_iter().filter(|(_, from_box)| !*from_box).map(|(line, _)| line).collect()
        } else {
            block.into_iter().map(|(line, _)| line).collect()
        };

        let joined = pieces.join(" ");
        let normalized = joined.split_whitespace().collect::<Vec<_>>().join(" ");
        if !normalized.is_empty() {
            return Some(clamp_excerpt_tail(&normalized, OUTPUT_EXCERPT_MAX_CHARS));
        }
    }

    None
}

fn is_common_output_noise(raw: &str, normalized: &str) -> bool {
    raw.starts_with('➜')
        || is_prompt_line(normalized)
        || is_prompt_footer_line(normalized)
        || normalized.starts_with("› ")
}

fn normalize_output_line(line: &str) -> String {
    line.trim_matches(|c: char| is_box_chrome_char(c) || c.is_whitespace())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_box_chrome_char(c: char) -> bool {
    matches!(
        c,
        '│' | '┃'
            | '╭'
            | '╮'
            | '╯'
            | '╰'
            | '╹'
            | '╻'
            | '┆'
            | '┊'
            | '─'
            | '═'
            | '█'
            | '▌'
            | '▐'
            | '▕'
            | '▎'
            | '▍'
            | '▉'
            | '▀'
            | '▄'
            | '▁'
            | '▔'
    )
}

fn clamp_excerpt_tail(value: &str, max_chars: usize) -> String {
    let chars: Vec<_> = value.chars().collect();
    if chars.len() <= max_chars {
        return value.to_string();
    }

    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }

    let start = chars.len().saturating_sub(max_chars - 3);
    format!("...{}", chars[start..].iter().collect::<String>())
}

fn extract_named_model_detail(output_tail: Option<&str>) -> Option<AgentDetail> {
    let output_tail = output_tail?;
    let label = output_tail.lines().rev().find_map(|line| {
        named_model_pattern()
            .captures(line.trim())
            .and_then(|captures| captures.name("model"))
            .map(|matched| normalize_detail_label(matched.as_str()))
            .filter(|label| !label.is_empty())
    })?;

    Some(AgentDetail { label, tone: AgentDetailTone::Neutral })
}

fn recent_nonempty_lines(output_tail: &str, limit: usize) -> Vec<&str> {
    let mut lines: Vec<_> =
        output_tail.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    if lines.len() > limit {
        lines.drain(..lines.len() - limit);
    }
    lines
}

fn retained_detail(previous: &SessionRecord) -> Option<Arc<AgentDetail>> {
    previous.detail.clone()
}

fn retained_output_excerpt(previous: &SessionRecord) -> Option<Arc<str>> {
    previous.output_excerpt.clone()
}

fn reuse_detail_arc(
    extracted: Option<AgentDetail>,
    previous: Option<&SessionRecord>,
) -> Option<Arc<AgentDetail>> {
    match extracted {
        Some(detail) => previous
            .and_then(|record| {
                record.detail.as_ref().filter(|existing| existing.as_ref() == &detail).cloned()
            })
            .or_else(|| Some(Arc::new(detail))),
        None => previous.and_then(retained_detail),
    }
}

fn reuse_output_excerpt_arc(
    extracted: Option<String>,
    previous: Option<&SessionRecord>,
) -> Option<Arc<str>> {
    match extracted {
        Some(output_excerpt) => previous
            .and_then(|record| {
                record
                    .output_excerpt
                    .as_ref()
                    .filter(|existing| existing.as_ref() == output_excerpt.as_str())
                    .cloned()
            })
            .or_else(|| Some(Arc::<str>::from(output_excerpt))),
        None => previous.and_then(retained_output_excerpt),
    }
}

fn full_output_fingerprint(output_tail: &str) -> Option<u64> {
    if output_tail.trim().is_empty() {
        return None;
    }

    let mut hasher = DefaultHasher::new();
    output_tail.hash(&mut hasher);
    Some(hasher.finish())
}

fn looks_like_waiting_prompt(recent_lines: &[&str], output_tail: &str) -> bool {
    looks_like_codex_bottom_prompt(recent_lines)
        || looks_like_amp_home_screen(recent_lines, output_tail)
        || looks_like_claude_setup_screen(output_tail)
        || looks_like_claude_prompt(recent_lines, output_tail)
        || looks_like_opencode_home_screen(recent_lines, output_tail)
        || looks_like_pi_prompt(recent_lines, output_tail)
}

fn looks_like_codex_bottom_prompt(recent_lines: &[&str]) -> bool {
    recent_lines
        .windows(2)
        .any(|window| is_prompt_line(window[0]) && is_prompt_footer_line(window[1]))
}

fn is_prompt_line(line: &str) -> bool {
    line.starts_with('\u{203a}')
}

fn is_prompt_footer_line(line: &str) -> bool {
    line.contains('\u{00b7}')
        && (line.contains("gpt-") || line.contains("~/") || line.contains("left"))
}

fn looks_like_amp_home_screen(recent_lines: &[&str], output_tail: &str) -> bool {
    let has_mode_skills = recent_lines.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        (lower.contains("smart") || lower.contains("rush")) && lower.contains("skills")
    });
    let has_workspace_footer =
        recent_lines.iter().any(|line| line.contains("~/") && line.contains('('));
    let has_amp_chrome = output_tail.contains("Welcome to")
        || output_tail.contains('╭')
        || output_tail.contains('╰');

    has_mode_skills && has_workspace_footer && has_amp_chrome
}

fn looks_like_claude_setup_screen(output_tail: &str) -> bool {
    let lower = output_tail.to_ascii_lowercase();
    lower.contains("welcome to claude code")
        && (lower.contains("choose the text style that looks best")
            || lower.contains("press enter to retry")
            || lower.contains("oauth error"))
}

fn looks_like_claude_prompt(recent_lines: &[&str], output_tail: &str) -> bool {
    let lower = output_tail.to_ascii_lowercase();

    lower.contains("select model") && lower.contains("enter to confirm")
        || recent_lines.iter().any(|line| line.starts_with('❯'))
            && recent_lines.iter().any(|line| {
                let lower = line.to_ascii_lowercase();
                lower.contains("/effort") || lower.contains("for shortcuts")
            })
}

fn looks_like_opencode_home_screen(recent_lines: &[&str], output_tail: &str) -> bool {
    let lower = output_tail.to_ascii_lowercase();
    let has_footer_hints = recent_lines.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("tab agents") && lower.contains("ctrl+p")
    });
    let has_opencode_chrome = lower.contains("ask anything")
        || recent_lines.iter().any(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("conversation title:")
                || lower.contains("opencode")
                || lower.contains("opencode zen")
        });

    has_footer_hints && has_opencode_chrome
}

fn looks_like_pi_prompt(recent_lines: &[&str], output_tail: &str) -> bool {
    output_tail.contains("PI Assistant")
        && output_tail.contains("Model:")
        && recent_lines.iter().any(|line| line.trim_start().starts_with("You:"))
}

fn looks_like_pi_idle(output_tail: &str) -> bool {
    let lower = output_tail.to_ascii_lowercase();
    (lower.contains("pi v") || lower.contains("warning: no models available"))
        && lower.contains("ctrl+l to select model")
        && pi_footer_pattern().is_match(output_tail)
}

fn output_has_recent_motion(
    output_fingerprint: Option<u64>,
    previous: Option<&SessionRecord>,
) -> bool {
    match (output_fingerprint, previous.and_then(|record| record.output_fingerprint)) {
        (Some(current), Some(previous)) => current != previous,
        _ => false,
    }
}

fn output_is_stable(output_fingerprint: Option<u64>, previous: Option<&SessionRecord>) -> bool {
    match (output_fingerprint, previous.and_then(|record| record.output_fingerprint)) {
        (Some(current), Some(previous)) => current == previous,
        _ => false,
    }
}

fn latest_recent_match<'a>(
    output_tail: &'a str,
    pattern: &'static Regex,
) -> Option<regex::Match<'a>> {
    pattern.find_iter(output_tail).filter(|matched| match_is_recent(output_tail, matched)).last()
}

fn match_is_recent(output_tail: &str, matched: &regex::Match<'_>) -> bool {
    output_tail.len().saturating_sub(matched.end()) <= STATUS_SIGNAL_WINDOW_BYTES
}

fn waiting_pattern() -> &'static Regex {
    static WAITING: OnceLock<Regex> = OnceLock::new();
    WAITING.get_or_init(|| {
        Regex::new(
            r"(?i)(waiting for input|press enter|continue\?|confirm|approve|y/n|select an option)",
        )
        .expect("waiting regex should compile")
    })
}

fn codex_card_model_pattern() -> &'static Regex {
    static CODEX_CARD_MODEL: OnceLock<Regex> = OnceLock::new();
    CODEX_CARD_MODEL.get_or_init(|| {
        Regex::new(r"(?im)model:\s+(?P<model>.+?)\s+/model to change\b")
            .expect("codex card model regex should compile")
    })
}

fn codex_footer_model_pattern() -> &'static Regex {
    static CODEX_FOOTER_MODEL: OnceLock<Regex> = OnceLock::new();
    CODEX_FOOTER_MODEL.get_or_init(|| {
        Regex::new(r"^(?P<model>[A-Za-z0-9][^·]+?)\s+·\s+\d+% left\b")
            .expect("codex footer model regex should compile")
    })
}

fn amp_mode_pattern() -> &'static Regex {
    static AMP_MODE: OnceLock<Regex> = OnceLock::new();
    AMP_MODE.get_or_init(|| {
        Regex::new(r"(?i)\b(?P<mode>smart|rush)\b").expect("amp mode regex should compile")
    })
}

fn named_model_pattern() -> &'static Regex {
    static NAMED_MODEL: OnceLock<Regex> = OnceLock::new();
    NAMED_MODEL.get_or_init(|| {
        Regex::new(r"(?i)\bmodel:\s*(?P<model>.+?)\s*$").expect("named model regex should compile")
    })
}

fn opencode_status_line_pattern() -> &'static Regex {
    static OPENCODE_STATUS_LINE: OnceLock<Regex> = OnceLock::new();
    OPENCODE_STATUS_LINE.get_or_init(|| {
        Regex::new(r"^(?:[┃│]\s*)?(?P<agent>[A-Za-z][A-Za-z0-9 _-]+?)\s{2,}(?P<context>.+?)\s*$")
            .expect("opencode status line regex should compile")
    })
}

fn opencode_build_line_pattern() -> &'static Regex {
    static OPENCODE_BUILD_LINE: OnceLock<Regex> = OnceLock::new();
    OPENCODE_BUILD_LINE.get_or_init(|| {
        Regex::new(
            r"(?i)▣\s+(?P<agent>[A-Za-z][A-Za-z0-9 _-]+)\s+·\s+(?P<model>[A-Za-z0-9._-]+)\s+·",
        )
        .expect("opencode build line regex should compile")
    })
}

fn claude_model_pattern() -> &'static Regex {
    static CLAUDE_MODEL: OnceLock<Regex> = OnceLock::new();
    CLAUDE_MODEL.get_or_init(|| {
        Regex::new(r"(?i)\b(?P<model>(?:Sonnet|Opus|Haiku)\s+[0-9.]+)\b")
            .expect("claude model regex should compile")
    })
}

fn claude_selected_model_pattern() -> &'static Regex {
    static CLAUDE_SELECTED_MODEL: OnceLock<Regex> = OnceLock::new();
    CLAUDE_SELECTED_MODEL.get_or_init(|| {
        Regex::new(r"(?i)^❯.*?\b(?P<model>(?:Sonnet|Opus|Haiku)\s+[0-9.]+)\b")
            .expect("claude selected model regex should compile")
    })
}

fn claude_model_set_pattern() -> &'static Regex {
    static CLAUDE_MODEL_SET: OnceLock<Regex> = OnceLock::new();
    CLAUDE_MODEL_SET.get_or_init(|| {
        Regex::new(r"(?i)\bset model to\s+(?P<model>(?:Sonnet|Opus|Haiku)\s+[0-9.]+)\b")
            .expect("claude model set regex should compile")
    })
}

fn claude_effort_pattern() -> &'static Regex {
    static CLAUDE_EFFORT: OnceLock<Regex> = OnceLock::new();
    CLAUDE_EFFORT.get_or_init(|| {
        Regex::new(r"(?i)\b(?P<effort>low|medium|high|max)\b(?:\s+effort|\s+·\s+/effort)")
            .expect("claude effort regex should compile")
    })
}

fn claude_elapsed_footer_pattern() -> &'static Regex {
    static CLAUDE_ELAPSED_FOOTER: OnceLock<Regex> = OnceLock::new();
    CLAUDE_ELAPSED_FOOTER.get_or_init(|| {
        Regex::new(r"(?i)^[a-z][a-z-]*(?: [a-z][a-z-]*){0,2} for \d+[smhd](?: \d+[smhd])*$")
            .expect("claude elapsed footer regex should compile")
    })
}

fn pi_footer_pattern() -> &'static Regex {
    static PI_FOOTER: OnceLock<Regex> = OnceLock::new();
    PI_FOOTER.get_or_init(|| {
        Regex::new(r"(?i)(?P<model>[A-Za-z0-9._-]+)\s+•\s+(?P<effort>low|medium|high|max|auto)\b")
            .expect("pi footer regex should compile")
    })
}

fn extract_claude_model_label(output_tail: &str) -> Option<String> {
    if let Some(label) = output_tail.lines().rev().find_map(|line| {
        claude_model_set_pattern()
            .captures(line.trim())
            .and_then(|captures| captures.name("model"))
            .map(|matched| normalize_detail_label(matched.as_str()))
            .filter(|label| !label.is_empty())
    }) {
        return Some(label);
    }

    if let Some(label) = output_tail.lines().find_map(|line| {
        claude_selected_model_pattern()
            .captures(line.trim())
            .and_then(|captures| captures.name("model"))
            .map(|matched| normalize_detail_label(matched.as_str()))
            .filter(|label| !label.is_empty())
    }) {
        return Some(label);
    }

    output_tail.lines().rev().find_map(|line| {
        claude_model_pattern()
            .captures(line.trim())
            .and_then(|captures| captures.name("model"))
            .map(|matched| normalize_detail_label(matched.as_str()))
            .filter(|label| !label.is_empty())
    })
}

fn extract_claude_effort_label(output_tail: &str) -> Option<String> {
    output_tail.lines().rev().find_map(|line| {
        claude_effort_pattern()
            .captures(line.trim())
            .and_then(|captures| captures.name("effort"))
            .map(|matched| matched.as_str().to_ascii_lowercase())
    })
}

fn extract_opencode_status_line(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with(['┃', '│']) {
        return None;
    }

    let trimmed = trimmed.trim_start_matches(['┃', '│']).trim_start();
    let captures = opencode_status_line_pattern().captures(trimmed)?;
    let agent = captures.name("agent")?.as_str().trim();
    let mut context = captures.name("context")?.as_str().trim().to_string();
    if context.ends_with("OpenCode Zen") {
        context = context.trim_end_matches("OpenCode Zen").trim().to_string();
    }
    if context.is_empty() {
        return None;
    }
    Some(normalize_detail_label(&format!("{agent} {context}")))
}

fn extract_opencode_build_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let captures = opencode_build_line_pattern().captures(trimmed)?;
    let agent = captures.name("agent")?.as_str().trim();
    let model = captures.name("model")?.as_str().trim();
    Some(normalize_detail_label(&format!("{agent} {model}")))
}

fn extract_pi_footer_model(output_tail: &str) -> Option<String> {
    output_tail.lines().rev().find_map(|line| {
        pi_footer_pattern()
            .captures(line.trim())
            .and_then(|captures| captures.name("model"))
            .map(|matched| normalize_detail_label(matched.as_str()))
            .filter(|label| !label.is_empty())
    })
}

fn extract_pi_footer_effort(output_tail: &str) -> Option<String> {
    output_tail.lines().rev().find_map(|line| {
        pi_footer_pattern()
            .captures(line.trim())
            .and_then(|captures| captures.name("effort"))
            .map(|matched| matched.as_str().to_ascii_lowercase())
    })
}

fn normalize_detail_label(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        classify_output_tail, extract_amp_detail, extract_amp_output_excerpt,
        extract_claude_detail, extract_claude_output_excerpt, extract_codex_detail,
        extract_codex_output_excerpt, extract_opencode_detail, extract_opencode_output_excerpt,
        extract_pi_detail, extract_pi_output_excerpt, AdapterRegistry, SessionTracker,
    };
    use crate::model::{AgentDetail, AgentDetailTone, AgentKind, SessionRecord, SessionStatus};
    use crate::tmux::PaneSnapshot;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn registry_detects_supported_commands_without_guessing_future_adapters() {
        let registry = AdapterRegistry::v1();
        let codex = snapshot("%1", "codex", false);
        let codex_variant = snapshot("%4", "codex-aarch64-a", false);
        let amp = snapshot("%2", "amp", false);
        let claude = snapshot("%3", "claude", false);
        let claude_title = snapshot_with_title("%21", "2.1.76", false, "✳ Claude Code");
        let claude_code = snapshot("%5", "claude-code", false);
        let opencode = snapshot("%6", "opencode", false);
        let opencode_title =
            snapshot_with_title("%22", "zsh", false, "OC | Conversation title: Quick test");
        let pi = snapshot("%7", "pi", false);
        let pi_title = snapshot_with_title("%23", "node", false, "π - worktree");
        let pi_agent = snapshot("%8", "pi-agent", false);

        assert_eq!(registry.detect_kind(&codex, None), Some(AgentKind::Codex));
        assert_eq!(registry.detect_kind(&codex_variant, None), Some(AgentKind::Codex));
        assert_eq!(registry.detect_kind(&amp, None), Some(AgentKind::Amp));
        assert_eq!(registry.detect_kind(&claude, None), Some(AgentKind::ClaudeCode));
        assert_eq!(registry.detect_kind(&claude_title, None), Some(AgentKind::ClaudeCode));
        assert_eq!(registry.detect_kind(&claude_code, None), Some(AgentKind::ClaudeCode));
        assert_eq!(registry.detect_kind(&opencode, None), Some(AgentKind::OpenCode));
        assert_eq!(registry.detect_kind(&opencode_title, None), Some(AgentKind::OpenCode));
        assert_eq!(registry.detect_kind(&pi, None), Some(AgentKind::Pi));
        assert_eq!(registry.detect_kind(&pi_title, None), Some(AgentKind::Pi));
        assert_eq!(registry.detect_kind(&pi_agent, None), Some(AgentKind::Pi));
    }

    #[test]
    fn tracker_marks_waiting_input_from_output_tail() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%7", "codex", false);
        let now = Instant::now();
        let output_tails = HashMap::from([(pane.pane_id.clone(), "Waiting for input".to_string())]);

        let records = tracker.refresh(&[pane], &output_tails, now);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::Codex);
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
        assert_eq!(records[0].retained_until, None);
    }

    #[test]
    fn tracker_marks_finished_when_agent_returns_to_shell() {
        let mut tracker = SessionTracker::with_retention(Duration::from_secs(30));
        let now = Instant::now();
        let running_pane = snapshot("%9", "codex", false);

        tracker.refresh(&[running_pane], &HashMap::new(), now);

        let shell_pane = snapshot("%9", "zsh", false);
        let records = tracker.refresh(
            std::slice::from_ref(&shell_pane),
            &HashMap::new(),
            now + Duration::from_secs(5),
        );

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::Codex);
        assert_eq!(records[0].status, SessionStatus::Finished);
        assert_eq!(records[0].pane.pane_id, shell_pane.pane_id);
        assert!(records[0].retained_until.is_some());
    }

    #[test]
    fn tracker_marks_finished_when_agent_returns_to_dash() {
        let mut tracker = SessionTracker::with_retention(Duration::from_secs(30));
        let now = Instant::now();
        let running_pane = snapshot("%10", "codex", false);

        tracker.refresh(&[running_pane], &HashMap::new(), now);

        let shell_pane = snapshot("%10", "dash", false);
        let records = tracker.refresh(
            std::slice::from_ref(&shell_pane),
            &HashMap::new(),
            now + Duration::from_secs(5),
        );

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, SessionStatus::Finished);
        assert_eq!(records[0].pane.pane_id, shell_pane.pane_id);
    }

    #[test]
    fn ambiguous_output_prefers_the_latest_prompt_state() {
        assert_eq!(
            classify_output_tail("completed. Press Enter to continue"),
            Some(SessionStatus::WaitingInput)
        );
        assert_eq!(
            classify_output_tail("Waiting for input\nFinished"),
            Some(SessionStatus::WaitingInput)
        );
    }

    #[test]
    fn stale_prompt_text_is_ignored_when_newer_output_follows() {
        let output_tail = format!(
            "Approve?\n{}\nstill processing work",
            "x".repeat(super::STATUS_SIGNAL_WINDOW_BYTES + 1)
        );

        assert_eq!(classify_output_tail(&output_tail), None);
    }

    #[test]
    fn stale_finished_text_is_ignored_when_newer_output_follows() {
        let output_tail = format!(
            "Completed successfully.\n{}\nstreaming more output",
            "x".repeat(super::STATUS_SIGNAL_WINDOW_BYTES + 1)
        );

        assert_eq!(classify_output_tail(&output_tail), None);
    }

    #[test]
    fn amp_defaults_waiting_when_old_prompt_text_is_outside_the_recent_window() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%12", "amp", false);
        let now = Instant::now();
        let output_tails = HashMap::from([(
            pane.pane_id.clone(),
            format!(
                "Approve?\n{}\nAmp is still working",
                "x".repeat(super::STATUS_SIGNAL_WINDOW_BYTES + 1)
            ),
        )]);

        let records = tracker.refresh(&[pane], &output_tails, now);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::Amp);
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn codex_bottom_prompt_marks_waiting_input() {
        let output_tail = "\
\n\
\u{203a} Write tests for @filename\n\
\n\
gpt-5.4 xhigh fast \u{00b7} 40% left \u{00b7} ~/Sites/ilmari\n";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn codex_working_banner_still_looks_like_waiting_prompt_on_first_load() {
        let output_tail = "\
• Working (2m 45s • esc to interrupt)

› Explain this codebase

gpt-5.4 xhigh fast · 20% left · ~/Sites/ilmari
";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn amp_home_screen_marks_waiting_input() {
        let output_tail = "\
Welcome to\n\
Ctrl+O for\n\
\n\
smart 30 skills\n\
~/Sites/ilmari (main)\n\
MCP 2 failed\n";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn amp_rush_home_screen_marks_waiting_input() {
        let output_tail = "\
Welcome to
Ctrl+O for

rush 4 skills
~/Sites/ilmari (main)
MCP 2 failed
";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn amp_conversation_prompt_marks_waiting_input() {
        let output_tail = "\
  ┃ c

  ✓ Thinking ▶

  Could you clarify what you'd like me to help with?

╭─26% of 168k · $0.45───────────────────────────────────────────────────────────────────smart──30 skills─╮
│                                                                                                        │
╰──────────────────────────────────────────────────────────────────────────────────~/Sites/ilmari (main)─╯
";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn claude_setup_screen_marks_waiting_input() {
        let output_tail = "\
Welcome to Claude Code v2.1.76
Choose the text style that looks best
with your terminal
❯ 1. Dark mode ✔
  2. Light mode
";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn claude_prompt_screen_marks_waiting_input() {
        let output_tail = "\
╭─── Claude Code v2.1.76 ──────────────────────────────────────────────╮
│      Sonnet 4.6 · Claude Pro · b@bnomei.com's Organization          │
╰───────────────────────────────────────────────────────────────────────╯

❯
  ? for shortcuts
  ◐ medium · /effort
";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn claude_model_menu_marks_waiting_input() {
        let output_tail = "\
  Select model
  ❯ 1. Default (recommended) ✔  Sonnet 4.6 · Best for everyday tasks
    2. Opus                     Opus 4.6 · Most capable for complex work
  ◐ Medium effort (default) ← → to adjust
  Enter to confirm · Esc to exit
";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn opencode_home_screen_marks_waiting_input() {
        let output_tail = "\
▄
OpenCode
Ask anything... \"Fix a TODO in the codebase\"
Build  Big Pickle OpenCode Zen
~/Sites/ilmari:main             ctrl+t variants  tab agents  ctrl+p co1.2.26
";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn opencode_conversation_prompt_marks_waiting_input() {
        let output_tail = "\
  ┃  # Conversation title: Quick test check-in                                       11,869  6% ($0.00)

     ▣  Build · big-pickle · 3.1s

  ┃  test 2
  ┃  Thinking: The user is just sending test messages. I should respond briefly.
     test 2 received
     ▣  Build · minimax-m2.5-free · 18.7s

  ┃  Build  MiniMax M2.5 Free OpenCode Zen
                                                                             tab agents  ctrl+p commands
";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn pi_prompt_marks_waiting_input() {
        let output_tail = "\
PI Assistant
Model: claude-opus-4-5
Session: /tmp/session.jsonl
Tools: read, bash, edit, write
You:
";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn pi_idle_screen_marks_waiting_input() {
        let output_tail = "\
pi v0.58.0
ctrl+l to select model
Warning: No models available. Use /login or set an API key environment variable.
~/Sites/frigg/specs/demo
claude-opus-4-5 • medium
";

        let pane = snapshot_with_title("%24", "node", false, "π - demo");
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let records = tracker.refresh(
            std::slice::from_ref(&pane),
            &HashMap::from([(pane.pane_id.clone(), output_tail.to_string())]),
            now,
        );

        assert_eq!(records[0].kind, AgentKind::Pi);
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn tracker_marks_pi_running_when_idle_footer_churns() {
        let pane = snapshot_with_title("%25", "node", false, "π - demo");
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let first_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
pi v0.58.0
ctrl+l to select model
Warning: No models available. Use /login or set an API key environment variable.
~/Sites/frigg/specs/demo
↑2.2k ↓64 $0.013 (sub) 1.1%/200k (auto)                                            claude-haiku-4-5 • medium
"
            .to_string(),
        )]);
        let second_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
pi v0.58.0
ctrl+l to select model
Warning: No models available. Use /login or set an API key environment variable.
~/Sites/frigg/specs/demo
↑9.4k ↓689 $0.023 (sub) 1.4%/200k (auto)                                           claude-haiku-4-5 • medium
"
            .to_string(),
        )]);

        let first = tracker.refresh(std::slice::from_ref(&pane), &first_output, now);
        assert_eq!(first[0].status, SessionStatus::WaitingInput);

        let second = tracker.refresh(
            std::slice::from_ref(&pane),
            &second_output,
            now + Duration::from_secs(5),
        );

        assert_eq!(second[0].status, SessionStatus::Running);
    }

    #[test]
    fn tracker_marks_pi_running_when_done_is_visible_and_footer_churns() {
        let pane = snapshot_with_title("%26", "node", false, "π - demo");
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let first_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
Hello! I'm ready to help.

Done! 🎉

~/Sites/frigg/specs/demo
↑2.2k ↓64 $0.013 (sub) 1.1%/200k (auto)                                            claude-haiku-4-5 • medium
"
            .to_string(),
        )]);
        let second_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
Hello! I'm ready to help.

Done! 🎉

~/Sites/frigg/specs/demo
↑9.4k ↓689 $0.023 (sub) 1.4%/200k (auto)                                           claude-haiku-4-5 • medium
"
            .to_string(),
        )]);

        let first = tracker.refresh(std::slice::from_ref(&pane), &first_output, now);
        assert_eq!(first[0].status, SessionStatus::WaitingInput);

        let second = tracker.refresh(
            std::slice::from_ref(&pane),
            &second_output,
            now + Duration::from_secs(5),
        );

        assert_eq!(second[0].status, SessionStatus::Running);
    }

    #[test]
    fn output_tail_capture_is_skipped_for_shell_return_paths() {
        let registry = AdapterRegistry::v1();
        let previous = SessionRecord {
            pane: snapshot("%1", "codex", false),
            kind: AgentKind::Codex,
            status: SessionStatus::Running,
            detail: None,
            output_excerpt: None,
            process_usage: None,
            output_fingerprint: None,
            last_changed_at: Instant::now(),
            last_seen_at: Instant::now(),
            retained_until: None,
        };

        assert!(registry.needs_output_tail(&snapshot("%1", "codex", false), Some(&previous)));
        assert!(!registry.needs_output_tail(&snapshot("%1", "zsh", false), Some(&previous)));
    }

    #[test]
    fn tracker_keeps_missing_panes_as_terminated_for_one_refresh_only() {
        let mut tracker = SessionTracker::with_retention(Duration::from_secs(30));
        let now = Instant::now();
        let pane = snapshot("%11", "amp", false);

        tracker.refresh(&[pane], &HashMap::new(), now);

        let terminated = tracker.refresh(&[], &HashMap::new(), now + Duration::from_secs(5));
        assert_eq!(terminated.len(), 1);
        assert_eq!(terminated[0].kind, AgentKind::Amp);
        assert_eq!(terminated[0].status, SessionStatus::Terminated);
        assert_eq!(terminated[0].retained_until, None);

        let expired = tracker.refresh(&[], &HashMap::new(), now + Duration::from_secs(10));
        assert!(expired.is_empty());
    }

    #[test]
    fn tracker_marks_running_when_full_pane_fingerprint_changes() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%13", "codex", false);
        let now = Instant::now();
        let waiting_prompt = HashMap::from([(
            pane.pane_id.clone(),
            "\
› Write tests for @filename

gpt-5.4 xhigh fast · 40% left · ~/Sites/ilmari
"
            .to_string(),
        )]);
        let running_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
processing next task chunk
streaming more output
still working
"
            .to_string(),
        )]);

        let first = tracker.refresh(std::slice::from_ref(&pane), &waiting_prompt, now);
        assert_eq!(first[0].status, SessionStatus::WaitingInput);

        let second = tracker.refresh(
            std::slice::from_ref(&pane),
            &running_output,
            now + Duration::from_secs(5),
        );

        assert_eq!(second[0].status, SessionStatus::Running);
        assert_ne!(second[0].output_fingerprint, first[0].output_fingerprint);
    }

    #[test]
    fn tracker_keeps_waiting_when_full_pane_fingerprint_stays_the_same() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%14", "codex", false);
        let now = Instant::now();
        let waiting_prompt = HashMap::from([(
            pane.pane_id.clone(),
            "\
› Use /skills to list available skills

gpt-5.4 xhigh fast · 78% left · ~/Sites/ilmari
"
            .to_string(),
        )]);

        tracker.refresh(std::slice::from_ref(&pane), &waiting_prompt, now);
        let second = tracker.refresh(
            std::slice::from_ref(&pane),
            &waiting_prompt,
            now + Duration::from_secs(5),
        );

        assert_eq!(second[0].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn tracker_marks_waiting_when_unclassified_full_pane_stays_the_same() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%15", "codex", false);
        let now = Instant::now();
        let output = HashMap::from([(
            pane.pane_id.clone(),
            "\
processing task chunk
still working through a long tool call
"
            .to_string(),
        )]);

        let first = tracker.refresh(std::slice::from_ref(&pane), &output, now);
        assert_eq!(first[0].status, SessionStatus::WaitingInput);

        let second =
            tracker.refresh(std::slice::from_ref(&pane), &output, now + Duration::from_secs(5));

        assert_eq!(second[0].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn tracker_keeps_waiting_when_output_tail_refresh_is_skipped() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%14", "codex", false);
        let now = Instant::now();
        let waiting_prompt = HashMap::from([(
            pane.pane_id.clone(),
            "\
› Use /skills to list available skills

gpt-5.4 xhigh fast · 78% left · ~/Sites/ilmari
"
            .to_string(),
        )]);

        tracker.refresh(std::slice::from_ref(&pane), &waiting_prompt, now);
        let second = tracker.refresh(
            std::slice::from_ref(&pane),
            &HashMap::new(),
            now + Duration::from_secs(5),
        );

        assert_eq!(second[0].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn tracker_keeps_running_when_prompt_tail_is_still_moving() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%16", "codex", false);
        let now = Instant::now();
        let first_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
processing task
› Write tests for @filename

gpt-5.4 xhigh fast · 40% left · ~/Sites/ilmari
"
            .to_string(),
        )]);
        let second_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
processing task chunk 2
› Write tests for @filename

gpt-5.4 xhigh fast · 39% left · ~/Sites/ilmari
"
            .to_string(),
        )]);

        let first = tracker.refresh(std::slice::from_ref(&pane), &first_output, now);
        assert_eq!(first[0].status, SessionStatus::WaitingInput);

        let second = tracker.refresh(
            std::slice::from_ref(&pane),
            &second_output,
            now + Duration::from_secs(5),
        );

        assert_eq!(second[0].status, SessionStatus::Running);
    }

    #[test]
    fn tracker_marks_waiting_on_first_load_when_pane_is_detected_but_unclassified() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%17", "codex", false);
        let now = Instant::now();
        let output = HashMap::from([(
            pane.pane_id.clone(),
            "\
processing task chunk
still working through a long tool call
"
            .to_string(),
        )]);

        let first = tracker.refresh(std::slice::from_ref(&pane), &output, now);

        assert_eq!(first[0].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn tracker_marks_running_when_codex_working_banner_changes() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%18", "codex", false);
        let now = Instant::now();
        let first_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
• Working (2m 45s • esc to interrupt)

› Explain this codebase

gpt-5.4 xhigh fast · 20% left · ~/Sites/ilmari
"
            .to_string(),
        )]);
        let second_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
• Working (2m 50s • esc to interrupt)

› Explain this codebase

gpt-5.4 xhigh fast · 20% left · ~/Sites/ilmari
"
            .to_string(),
        )]);

        let first = tracker.refresh(std::slice::from_ref(&pane), &first_output, now);
        assert_eq!(first[0].status, SessionStatus::WaitingInput);

        let second = tracker.refresh(
            std::slice::from_ref(&pane),
            &second_output,
            now + Duration::from_secs(5),
        );

        assert_eq!(second[0].status, SessionStatus::Running);
    }

    #[test]
    fn codex_detail_extracts_from_model_card_line() {
        let output_tail = "\
│ model:     gpt-5.4 xhigh   fast   /model to change │
│ something else │
";

        assert_eq!(
            extract_codex_detail(Some(output_tail)),
            Some(AgentDetail {
                label: "gpt-5.4 xhigh fast".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn codex_detail_extracts_from_footer_line() {
        let output_tail = "\
› /model
gpt-5.4 xhigh fast · 100% left · ~/Sites/ilmari
";

        assert_eq!(
            extract_codex_detail(Some(output_tail)),
            Some(AgentDetail {
                label: "gpt-5.4 xhigh fast".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn codex_output_excerpt_skips_prompt_and_footer() {
        let output_tail = "\
Here is the first part of the answer.
The tracker now keeps waiting panes stable.
› Write tests for @filename

gpt-5.4 xhigh fast · 40% left · ~/Sites/ilmari
";

        assert_eq!(
            extract_codex_output_excerpt(Some(output_tail)),
            Some(
                "... is the first part of the answer. The tracker now keeps waiting panes stable."
                    .to_string()
            )
        );
    }

    #[test]
    fn amp_detail_extracts_smart_and_rush_modes() {
        let smart = "\
╭─────────────────────smart──30 skills─╮
╰────────────────~/Sites/ilmari (main)─╯
";
        let rush = "\
╭──────────────────────rush──4 skills─╮
╰────────────────~/Sites/ilmari (main)─╯
";

        assert_eq!(
            extract_amp_detail(Some(smart)),
            Some(AgentDetail { label: "smart".to_string(), tone: AgentDetailTone::Positive })
        );
        assert_eq!(
            extract_amp_detail(Some(rush)),
            Some(AgentDetail { label: "rush".to_string(), tone: AgentDetailTone::Warning })
        );
    }

    #[test]
    fn amp_output_excerpt_skips_footer_chrome() {
        let output_tail = "\
  ┃ c

  ✓ Thinking ▶

  Could you clarify what you'd like me to help with?

╭─26% of 168k · $0.45───────────────────────────────────────────────────────────────────smart──30 skills─╮
╰──────────────────────────────────────────────────────────────────────────────────~/Sites/ilmari (main)─╯
";

        assert_eq!(
            extract_amp_output_excerpt(Some(output_tail)),
            Some("Could you clarify what you'd like me to help with?".to_string())
        );
    }

    #[test]
    fn claude_detail_extracts_from_named_model_line() {
        assert_eq!(
            extract_claude_detail(Some("Model: claude-sonnet-4-5")),
            Some(AgentDetail {
                label: "claude-sonnet-4-5".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn claude_detail_extracts_model_and_effort_from_live_prompt() {
        let output_tail = "\
╭─── Claude Code v2.1.76 ──────────────────────────────────────────────╮
│      Sonnet 4.6 · Claude Pro · b@bnomei.com's Organization          │
╰───────────────────────────────────────────────────────────────────────╯

❯
  ? for shortcuts
  ◐ medium · /effort
";

        assert_eq!(
            extract_claude_detail(Some(output_tail)),
            Some(AgentDetail {
                label: "Sonnet 4.6 medium".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn claude_output_excerpt_skips_prompt_chrome() {
        let output_tail = "\
╭─── Claude Code v2.1.76 ──────────────────────────────────────────────╮
│      Sonnet 4.6 · Claude Pro · Org                                  │
╰───────────────────────────────────────────────────────────────────────╯

I updated the layout handling and added a regression test.

❯
  ? for shortcuts
  ◐ medium · /effort
";

        assert_eq!(
            extract_claude_output_excerpt(Some(output_tail)),
            Some("I updated the layout handling and added a regression test.".to_string())
        );
    }

    #[test]
    fn claude_output_excerpt_ignores_generic_elapsed_footer_labels() {
        let output_tail = "\
I tightened the pane classifier and moved the time column to the left.

Brewed for 41s
";

        assert_eq!(
            extract_claude_output_excerpt(Some(output_tail)),
            Some(
                "I tightened the pane classifier and moved the time column to the left."
                    .to_string()
            )
        );
    }

    #[test]
    fn claude_detail_extracts_from_model_menu() {
        let output_tail = "\
  Select model
  ❯ 1. Default (recommended) ✔  Sonnet 4.6 · Best for everyday tasks
    2. Opus                     Opus 4.6 · Most capable for complex work
  ◐ Medium effort (default) ← → to adjust
  Enter to confirm · Esc to exit
";

        assert_eq!(
            extract_claude_detail(Some(output_tail)),
            Some(AgentDetail {
                label: "Sonnet 4.6 medium".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn claude_detail_extracts_from_model_change_confirmation() {
        let output_tail = "\
❯ /model
  ⎿  Set model to Sonnet 4.6 (default)
";

        assert_eq!(
            extract_claude_detail(Some(output_tail)),
            Some(AgentDetail { label: "Sonnet 4.6".to_string(), tone: AgentDetailTone::Neutral })
        );
    }

    #[test]
    fn opencode_detail_extracts_from_live_status_line() {
        let output_tail = "  ┃  Build  Big Pickle OpenCode Zen";

        assert_eq!(
            extract_opencode_detail(Some(output_tail)),
            Some(AgentDetail {
                label: "Build Big Pickle".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn opencode_detail_ignores_footer_hint_lines() {
        let output_tail = "\
  ┃  Build  Big Pickle OpenCode Zen
~/Sites/ilmari:main             ctrl+t variants  tab agents  ctrl+p commands
";

        assert_eq!(
            extract_opencode_detail(Some(output_tail)),
            Some(AgentDetail {
                label: "Build Big Pickle".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn opencode_output_excerpt_prefers_visible_reply_lines() {
        let output_tail = "\
  ┃  # Conversation title: Quick test check-in
  ┃  test 2
  ┃  Thinking: The user is just sending test messages. I should respond briefly.
     test 2 received
     ▣  Build · minimax-m2.5-free · 18.7s

  ┃  Build  MiniMax M2.5 Free OpenCode Zen
                                                                             tab agents  ctrl+p commands
";

        assert_eq!(
            extract_opencode_output_excerpt(Some(output_tail)),
            Some("test 2 received".to_string())
        );
    }

    #[test]
    fn opencode_output_excerpt_ignores_footer_after_latest_reply_block() {
        let output_tail = "\
  ┃  okies

  ┃  Thinking: The user is just saying \"okies\" which is an informal acknowledgment.
     Got it. What would you like me to help with?
     ▣  Build · minimax-m2.5-free · 10.2s

  ┃  Build  MiniMax M2.5 Free OpenCode Zen
                                                                             tab agents  ctrl+p commands
";

        assert_eq!(
            extract_opencode_output_excerpt(Some(output_tail)),
            Some("Got it. What would you like me to help with?".to_string())
        );
    }

    #[test]
    fn pi_detail_extracts_from_live_footer() {
        assert_eq!(
            extract_pi_detail(Some(
                "↑2.2k ↓64 $0.013 (sub) 1.1%/200k (auto)       claude-opus-4-5 • medium"
            )),
            Some(AgentDetail {
                label: "claude-opus-4-5 medium".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn pi_output_excerpt_skips_header_and_footer() {
        let output_tail = "\
PI Assistant
Model: claude-opus-4-5
Session: /tmp/session.jsonl
Tools: read, bash, edit, write

I can help once a model is configured.

You:
claude-opus-4-5 • medium
";

        assert_eq!(
            extract_pi_output_excerpt(Some(output_tail)),
            Some("I can help once a model is configured.".to_string())
        );
    }

    #[test]
    fn pi_output_excerpt_prefers_latest_reply_block_before_footer() {
        let output_tail = "\
test

Hello! I'm ready to help. What would you like to work on?

Model: claude-haiku-4-5
~/Sites/frigg/specs/demo (main)
↑2.2k ↓64 $0.013 (sub) 1.1%/200k (auto)             claude-haiku-4-5 • medium
pi v0.58.0
escape to interrupt
ctrl+l to select model

yes

The user said \"yes\". I should ask what they need next.
Great! What would you like me to help you with?
Just let me know what you need!

Model: claude-haiku-4-5
";

        assert_eq!(
            extract_pi_output_excerpt(Some(output_tail)),
            Some(
                "...eat! What would you like me to help you with? Just let me know what you need!"
                    .to_string()
            )
        );
    }

    #[test]
    fn claude_footer_only_effort_does_not_replace_model_detail() {
        assert_eq!(extract_claude_detail(Some("◐ medium · /effort")), None);
    }

    #[test]
    fn tracker_retains_previous_detail_when_new_tail_lacks_it() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%15", "codex", false);
        let now = Instant::now();
        let first = tracker.refresh(
            std::slice::from_ref(&pane),
            &HashMap::from([(
                pane.pane_id.clone(),
                "gpt-5.4 xhigh fast · 100% left · ~/Sites/ilmari".to_string(),
            )]),
            now,
        );

        assert_eq!(
            first[0].detail,
            Some(
                AgentDetail {
                    label: "gpt-5.4 xhigh fast".to_string(),
                    tone: AgentDetailTone::Neutral,
                }
                .into()
            )
        );

        let second = tracker.refresh(
            std::slice::from_ref(&pane),
            &HashMap::from([("%15".to_string(), "streaming more output".to_string())]),
            now + Duration::from_secs(5),
        );

        assert_eq!(
            second[0].detail,
            Some(
                AgentDetail {
                    label: "gpt-5.4 xhigh fast".to_string(),
                    tone: AgentDetailTone::Neutral,
                }
                .into()
            )
        );
    }

    #[test]
    fn tracker_reuses_detail_and_output_excerpt_arcs_when_values_do_not_change() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%15", "codex", false);
        let now = Instant::now();
        let output_tail =
            "gpt-5.4 xhigh fast · 100% left · ~/Sites/ilmari\nHello from the current turn.";
        let first = tracker.refresh(
            std::slice::from_ref(&pane),
            &HashMap::from([(pane.pane_id.clone(), output_tail.to_string())]),
            now,
        );
        let second = tracker.refresh(
            std::slice::from_ref(&pane),
            &HashMap::from([(pane.pane_id.clone(), output_tail.to_string())]),
            now + Duration::from_secs(5),
        );

        assert!(Arc::ptr_eq(
            first[0].detail.as_ref().expect("detail should exist"),
            second[0].detail.as_ref().expect("detail should exist"),
        ));
        assert!(Arc::ptr_eq(
            first[0].output_excerpt.as_ref().expect("output excerpt should exist"),
            second[0].output_excerpt.as_ref().expect("output excerpt should exist"),
        ));
    }

    fn snapshot(pane_id: &str, pane_current_command: &str, pane_dead: bool) -> PaneSnapshot {
        snapshot_with_title(pane_id, pane_current_command, pane_dead, "worker")
    }

    fn snapshot_with_title(
        pane_id: &str,
        pane_current_command: &str,
        pane_dead: bool,
        pane_title: &str,
    ) -> PaneSnapshot {
        PaneSnapshot::parse(&format!(
            "{pane_id}\t301\t$5\tclient\t@8\tagents\t{}\t/Users/bnomei/Sites/ilmari\t{pane_current_command}\t{pane_title}",
            if pane_dead { 1 } else { 0 }
        ))
        .expect("pane snapshot should parse")
    }
}
