use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::model::{AgentDetail, AgentDetailTone, AgentKind, SessionRecord, SessionStatus};
use crate::tmux::PaneSnapshot;

mod adapters;
use adapters::{
    AiderAdapter, AmpAdapter, AntigravityAdapter, AuggieAdapter, ClaudeCodeAdapter,
    ClineCliAdapter, CodexAdapter, CursorCliAdapter, GeminiAdapter, GitHubCopilotCliAdapter,
    GooseCliAdapter, GrokAdapter, KiroCliAdapter, OpenCodeAdapter, OpenHandsCliAdapter, PiAdapter,
};

pub const DEFAULT_RETENTION: Duration = Duration::from_secs(30);
const STATUS_SIGNAL_WINDOW_BYTES: usize = 240;
const AMP_STATUS_SIGNAL_WINDOW_LINES: usize = 12;
const AMP_STATUS_SIGNAL_MAX_CHARS: usize = 40;
const PROMPT_LINE_WINDOW: usize = 6;
const OUTPUT_EXCERPT_MAX_CHARS: usize = 80;

pub trait AgentAdapter {
    fn kind(&self) -> AgentKind;
    fn detect(&self, pane: &PaneSnapshot) -> bool;
    fn detect_output(&self, _pane: &PaneSnapshot, _output_tail: &str) -> bool {
        false
    }
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
            adapters: all_adapters()
                .into_iter()
                .filter(|adapter| adapter.kind().is_enabled())
                .collect(),
        }
    }

    #[cfg(test)]
    fn all_for_tests() -> Self {
        Self { adapters: all_adapters() }
    }

    #[cfg(test)]
    fn enabled_kinds(&self) -> Vec<AgentKind> {
        self.adapters.iter().map(|adapter| adapter.kind()).collect()
    }
}

fn all_adapters() -> Vec<Box<dyn AgentAdapter>> {
    vec![
        Box::new(CodexAdapter),
        Box::new(AmpAdapter),
        Box::new(ClaudeCodeAdapter),
        Box::new(OpenCodeAdapter),
        Box::new(PiAdapter),
        Box::new(GeminiAdapter),
        Box::new(AntigravityAdapter),
        Box::new(AuggieAdapter),
        Box::new(GrokAdapter),
        Box::new(GitHubCopilotCliAdapter),
        Box::new(CursorCliAdapter),
        Box::new(AiderAdapter),
        Box::new(ClineCliAdapter),
        Box::new(GooseCliAdapter),
        Box::new(KiroCliAdapter),
        Box::new(OpenHandsCliAdapter),
    ]
}

impl AdapterRegistry {
    #[cfg(test)]
    pub fn detect_kind(
        &self,
        pane: &PaneSnapshot,
        previous: Option<&SessionRecord>,
    ) -> Option<AgentKind> {
        self.select_adapter(pane, None, previous, None).map(AgentAdapter::kind)
    }

    pub fn needs_output_tail(
        &self,
        pane: &PaneSnapshot,
        previous: Option<&SessionRecord>,
        identity_hint: Option<AgentKind>,
    ) -> bool {
        !pane.pane_dead
            && (identity_hint.is_some()
                || (!is_shell_command(&pane.pane_current_command)
                    && (self.select_adapter(pane, None, previous, None).is_some()
                        || is_runtime_wrapped_agent_candidate(&pane.pane_current_command)
                        || is_claude_output_confirmation_candidate(pane))))
    }

    fn select_adapter<'a>(
        &'a self,
        pane: &PaneSnapshot,
        output_tail: Option<&str>,
        previous: Option<&SessionRecord>,
        identity_hint: Option<AgentKind>,
    ) -> Option<&'a dyn AgentAdapter> {
        if let Some(identity_hint) = identity_hint {
            if let Some(adapter) = self
                .adapters
                .iter()
                .find(|adapter| adapter.kind() == identity_hint)
                .map(Box::as_ref)
            {
                return Some(adapter);
            }
        }

        if let Some(previous) = previous {
            if let Some(adapter) = self
                .adapters
                .iter()
                .find(|adapter| adapter.kind() == previous.kind)
                .map(Box::as_ref)
            {
                if pane.pane_dead
                    || adapter.detect(pane)
                    || output_tail
                        .is_some_and(|output_tail| adapter.detect_output(pane, output_tail))
                    || is_claude_spinner_continuation(pane, previous)
                    || is_shell_command(&pane.pane_current_command)
                {
                    return Some(adapter);
                }
            }
        }

        self.adapters
            .iter()
            .find(|adapter| {
                adapter.detect(pane)
                    || output_tail
                        .is_some_and(|output_tail| adapter.detect_output(pane, output_tail))
            })
            .map(Box::as_ref)
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

    #[cfg(test)]
    pub fn refresh(
        &mut self,
        panes: &[PaneSnapshot],
        output_tails: &HashMap<String, String>,
        now: Instant,
    ) -> Vec<SessionRecord> {
        self.refresh_with_process_kinds(panes, output_tails, &HashMap::new(), now)
    }

    pub fn refresh_with_process_kinds(
        &mut self,
        panes: &[PaneSnapshot],
        output_tails: &HashMap<String, String>,
        process_kinds: &HashMap<String, AgentKind>,
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
                process_kinds.get(&pane.pane_id).copied(),
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
        identity_hint: Option<AgentKind>,
        now: Instant,
    ) -> Option<SessionRecord> {
        let adapter = self.registry.select_adapter(pane, output_tail, previous, identity_hint)?;
        let kind = adapter.kind();
        let output_fingerprint =
            output_tail.and_then(|output_tail| output_fingerprint_for_kind(kind, output_tail));
        let status = adapter.classify(pane, output_tail, output_fingerprint, previous);
        let detail = adapter.extract_detail(output_tail, previous);
        let output_excerpt = adapter.extract_output_excerpt(output_tail, previous);
        let retained_until = retention_deadline(previous, status, self.retention, now)?;
        let last_changed_at = match previous {
            Some(previous) if previous.kind == kind && previous.status == status => {
                previous.last_changed_at
            }
            _ => now,
        };

        Some(SessionRecord {
            pane: pane.clone(),
            kind,
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

fn classify_supported_session(
    adapter: &dyn AgentAdapter,
    pane: &PaneSnapshot,
    output_tail: Option<&str>,
    output_fingerprint: Option<u64>,
    previous: Option<&SessionRecord>,
) -> SessionStatus {
    classify_supported_session_with_tail_classifier(
        adapter,
        pane,
        output_tail,
        output_fingerprint,
        previous,
        classify_output_tail,
    )
}

fn classify_antigravity_session(
    adapter: &dyn AgentAdapter,
    pane: &PaneSnapshot,
    output_tail: Option<&str>,
    output_fingerprint: Option<u64>,
    previous: Option<&SessionRecord>,
) -> SessionStatus {
    classify_supported_session_with_tail_classifier(
        adapter,
        pane,
        output_tail,
        output_fingerprint,
        previous,
        classify_antigravity_output_tail,
    )
}

fn classify_grok_session(
    adapter: &dyn AgentAdapter,
    pane: &PaneSnapshot,
    output_tail: Option<&str>,
    output_fingerprint: Option<u64>,
    previous: Option<&SessionRecord>,
) -> SessionStatus {
    if pane.pane_dead {
        return SessionStatus::Terminated;
    }

    if previous.is_some() && is_shell_command(&pane.pane_current_command) {
        return SessionStatus::Finished;
    }

    if let Some(retained_status) = retained_status_without_output_tail(output_tail, previous) {
        return retained_status;
    }

    let output_tail = output_tail.unwrap_or_default();

    if looks_like_grok_active_waiting_footer(output_tail) {
        return SessionStatus::Running;
    }

    if let Some(status) = classify_grok_output_tail(output_tail) {
        return status;
    }

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

fn classify_supported_session_with_tail_classifier<F>(
    adapter: &dyn AgentAdapter,
    pane: &PaneSnapshot,
    output_tail: Option<&str>,
    output_fingerprint: Option<u64>,
    previous: Option<&SessionRecord>,
    classify_tail: F,
) -> SessionStatus
where
    F: FnOnce(&str) -> Option<SessionStatus>,
{
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

    if let Some(status) = classify_tail(output_tail) {
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

fn is_runtime_wrapped_agent_candidate(command: &str) -> bool {
    command_equals_any(command, &["node"])
}

fn is_claude_output_confirmation_candidate(pane: &PaneSnapshot) -> bool {
    !command_matches_non_claude_agent(&pane.pane_current_command)
        && is_claude_spinner_title(&pane.pane_title)
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
    command
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_start_matches('.')
        .to_ascii_lowercase()
}

fn is_claude_spinner_continuation(pane: &PaneSnapshot, previous: &SessionRecord) -> bool {
    previous.kind == AgentKind::ClaudeCode
        && !is_shell_command(&pane.pane_current_command)
        && !command_matches_non_claude_agent(&pane.pane_current_command)
        && is_claude_spinner_title(&pane.pane_title)
}

fn is_claude_direct_title(title: &str) -> bool {
    pane_title_contains(title, "claude code") || title_starts_with(title, '✳')
}

fn is_claude_spinner_title(title: &str) -> bool {
    title.trim_start().chars().next().is_some_and(is_claude_spinner_glyph)
}

fn is_claude_spinner_glyph(c: char) -> bool {
    matches!(
        c,
        '⠋' | '⠙'
            | '⠹'
            | '⠸'
            | '⠼'
            | '⠴'
            | '⠦'
            | '⠧'
            | '⠇'
            | '⠏'
            | '⠐'
            | '⠈'
            | '⠁'
            | '⠄'
            | '⠠'
            | '⠂'
            | '⡀'
            | '⢀'
    )
}

fn title_starts_with(title: &str, expected: char) -> bool {
    title.trim_start().starts_with(expected)
}

fn command_matches_non_claude_agent(command: &str) -> bool {
    command_matches(command, "codex")
        || command_matches(command, "amp")
        || command_matches(command, "opencode")
        || command_equals_any(command, &["pi", "pi-agent"])
        || command_matches(command, "gemini")
        || command_matches(command, "agy")
        || command_matches(command, "auggie")
        || command_matches(command, "grok")
        || command_matches(command, "copilot")
        || command_matches(command, "cursor")
        || command_matches(command, "aider")
        || command_matches(command, "cline")
        || command_matches(command, "goose")
        || command_matches(command, "kiro-cli")
        || command_matches(command, "openhands")
}

fn looks_like_gemini_output(output_tail: &str) -> bool {
    let lower = output_tail.to_ascii_lowercase();
    lower.contains("gemini cli v")
        || lower.contains("gemini code assist")
        || extract_gemini_footer_model(output_tail).is_some()
}

fn looks_like_antigravity_output(output_tail: &str) -> bool {
    let lower = output_tail.to_ascii_lowercase();
    lower.contains("antigravity cli")
        || looks_like_antigravity_active(output_tail)
        || looks_like_antigravity_permission_prompt(output_tail)
        || extract_antigravity_footer_model(output_tail).is_some()
}

fn looks_like_auggie_output(output_tail: &str) -> bool {
    let lower = output_tail.to_ascii_lowercase();
    lower.contains("get started with auggie")
        || lower.contains("for automation, use 'auggie --print")
        || lower.contains("tip: use 'auggie session continue'")
}

fn classify_output_tail(output_tail: &str) -> Option<SessionStatus> {
    let recent_lines = recent_nonempty_lines(output_tail, PROMPT_LINE_WINDOW);
    if looks_like_waiting_prompt(&recent_lines, output_tail) {
        return Some(SessionStatus::WaitingInput);
    }

    latest_recent_match(output_tail, waiting_pattern()).map(|_| SessionStatus::WaitingInput)
}

fn classify_antigravity_output_tail(output_tail: &str) -> Option<SessionStatus> {
    if looks_like_antigravity_active(output_tail) {
        return Some(SessionStatus::Running);
    }

    if looks_like_antigravity_permission_prompt(output_tail) {
        return Some(SessionStatus::WaitingInput);
    }

    let recent_lines = recent_nonempty_lines(output_tail, PROMPT_LINE_WINDOW);
    if looks_like_antigravity_prompt(&recent_lines, output_tail) {
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
                extract_codex_status_model(line.trim())
                    .map(|label| normalize_detail_label(&label))
                    .filter(|label| !label.is_empty())
            })
        })?;

    Some(AgentDetail { label, tone: AgentDetailTone::Neutral })
}

fn extract_codex_status_model(line: &str) -> Option<String> {
    if !line.contains("gpt-") || !(line.contains('·') || line.contains("/model to change")) {
        return None;
    }

    codex_status_model_pattern()
        .captures(line)
        .and_then(|captures| captures.name("model"))
        .map(|matched| matched.as_str().trim().to_string())
}

fn extract_amp_detail(output_tail: Option<&str>) -> Option<AgentDetail> {
    let output_tail = output_tail?;
    let label = output_tail.lines().rev().find_map(|line| {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("skills") {
            amp_mode_pattern()
                .captures(trimmed)
                .and_then(|captures| captures.name("mode"))
                .map(|matched| normalize_amp_mode_label(matched.as_str(), false))
        } else {
            amp_compact_mode_pattern().captures(trimmed).and_then(|captures| {
                let has_bolt = captures.name("bolt").is_some();
                let mode = captures.name("mode")?;
                let label = normalize_amp_mode_label(mode.as_str(), has_bolt);
                (has_bolt || label != "rush").then_some(label)
            })
        }
    })?;

    let mode = label.strip_prefix('↯').unwrap_or(&label);
    let tone = match mode {
        "deep" | "deep²" => AgentDetailTone::AmpDeep,
        "smart" => AgentDetailTone::AmpSmart,
        "rush" => AgentDetailTone::AmpRush,
        _ => AgentDetailTone::Neutral,
    };

    Some(AgentDetail { label, tone })
}

fn normalize_amp_mode_label(mode: &str, has_bolt: bool) -> String {
    let normalized = match mode.to_ascii_lowercase().as_str() {
        "deep2" => "deep²".to_string(),
        other => other.to_string(),
    };

    if has_bolt {
        format!("↯{normalized}")
    } else {
        normalized
    }
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

fn extract_gemini_detail(output_tail: Option<&str>) -> Option<AgentDetail> {
    let output_tail = output_tail?;
    let label = extract_gemini_footer_model(output_tail)?;

    Some(AgentDetail { label, tone: AgentDetailTone::Neutral })
}

fn extract_antigravity_detail(output_tail: Option<&str>) -> Option<AgentDetail> {
    let output_tail = output_tail?;
    let label = extract_antigravity_footer_model(output_tail)?;

    Some(AgentDetail { label, tone: AgentDetailTone::Neutral })
}

fn extract_gemini_footer_model(output_tail: &str) -> Option<String> {
    output_tail
        .lines()
        .rev()
        .find_map(|line| extract_gemini_inline_footer_model(line.trim()))
        .or_else(|| extract_gemini_table_footer_model(output_tail))
}

fn extract_gemini_inline_footer_model(line: &str) -> Option<String> {
    gemini_footer_model_pattern()
        .captures(line)
        .and_then(|captures| captures.name("model"))
        .map(|matched| normalize_detail_label(matched.as_str()))
        .filter(|label| !label.is_empty())
}

fn extract_gemini_table_footer_model(output_tail: &str) -> Option<String> {
    let lines: Vec<_> = output_tail.lines().collect();
    let mut latest = None;

    for (index, line) in lines.iter().enumerate() {
        let normalized = normalize_output_line(line.trim());
        if !is_gemini_table_footer_header(&normalized) {
            continue;
        }

        for candidate in lines.iter().skip(index + 1).take(3) {
            let normalized = normalize_output_line(candidate.trim());
            if normalized.is_empty() {
                continue;
            }

            if let Some(model) = extract_gemini_table_footer_row_model(&normalized) {
                latest = Some(model);
                break;
            }
        }
    }

    latest
}

fn extract_gemini_table_footer_row_model(normalized: &str) -> Option<String> {
    gemini_table_footer_model_pattern()
        .captures(normalized)
        .and_then(|captures| captures.name("model"))
        .map(|matched| normalize_detail_label(matched.as_str().trim_end_matches('…')))
        .filter(|label| !label.is_empty())
}

fn is_gemini_table_footer_header(normalized: &str) -> bool {
    let lower = normalized.to_ascii_lowercase();
    lower.contains("workspace") && lower.contains("sandbox") && lower.contains("/model")
}

fn extract_antigravity_footer_model(output_tail: &str) -> Option<String> {
    output_tail.lines().rev().find_map(|line| {
        antigravity_footer_model_pattern()
            .captures(line.trim())
            .and_then(|captures| captures.name("model"))
            .map(|matched| normalize_detail_label(matched.as_str()))
            .filter(|label| !label.is_empty())
    })
}

fn extract_auggie_detail(output_tail: Option<&str>) -> Option<AgentDetail> {
    let output_tail = output_tail?;
    let label = extract_auggie_selected_model(output_tail)
        .or_else(|| extract_auggie_using_model(output_tail))
        .or_else(|| extract_auggie_footer_model(output_tail))?;

    Some(AgentDetail { label, tone: AgentDetailTone::Neutral })
}

fn extract_grok_detail(output_tail: Option<&str>) -> Option<AgentDetail> {
    let output_tail = output_tail?;
    let label = output_tail.lines().rev().find_map(|line| {
        let trimmed = line.trim();
        let normalized = normalize_output_line(line.trim());
        let lower = normalized.to_ascii_lowercase();

        if let Some(label) = extract_grok_footer_model(trimmed, &normalized) {
            return Some(label);
        }

        if !(lower.starts_with("grok ") || lower.contains("i'm grok") || lower.contains("i’m grok"))
        {
            return None;
        }

        grok_model_pattern()
            .captures(&normalized)
            .and_then(|captures| captures.name("model"))
            .map(|matched| normalize_detail_label(matched.as_str()))
            .filter(|label| !label.is_empty())
    });
    label.map(|label| AgentDetail { label, tone: AgentDetailTone::Neutral })
}

fn extract_grok_footer_model(raw: &str, normalized: &str) -> Option<String> {
    if !raw.trim_start().starts_with('╰') && !normalized.contains('·') {
        return None;
    }

    grok_footer_model_pattern()
        .captures(normalized)
        .and_then(|captures| captures.name("model"))
        .map(|matched| normalize_detail_label(matched.as_str()))
        .filter(|label| !label.is_empty())
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
    extract_output_excerpt_from_tail(output_tail, is_amp_output_noise)
}

fn is_amp_output_noise(raw: &str, normalized: &str) -> bool {
    let lower = normalized.to_ascii_lowercase();
    let raw_lower = raw.to_ascii_lowercase();
    is_common_output_noise(raw, normalized)
        || raw_lower.contains("welcome to")
        || raw.trim_start().starts_with(['╭', '╰'])
        || (amp_mode_label_in_text(&lower) && lower.contains("skills"))
        || (normalized.contains("~/") && normalized.contains('('))
        || raw.contains('✓') && lower.contains("thinking")
        || lower.contains("of 168k")
        || (normalized.chars().count() == 1 && raw.trim_start().starts_with(['│', '┃']))
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

fn extract_gemini_output_excerpt(output_tail: Option<&str>) -> Option<String> {
    let output_tail = output_tail?;
    extract_output_excerpt_from_tail(output_tail, |raw, normalized| {
        let lower = normalized.to_ascii_lowercase();
        is_common_output_noise(raw, normalized)
            || looks_like_gemini_output(raw)
            || lower.contains("logged in with google")
            || lower.starts_with("? for shortcuts")
            || is_gemini_shortcut_footer(&lower)
            || lower.contains("gemini.md file")
            || lower.contains("no sandbox (see /docs)")
            || lower.contains("request cancelled")
            || lower.starts_with("type your message or @path/to/file")
            || gemini_footer_model_pattern().is_match(normalized)
            || is_gemini_table_footer_header(normalized)
            || extract_gemini_table_footer_row_model(normalized).is_some()
            || raw.trim_start().starts_with('>')
    })
}

fn extract_antigravity_output_excerpt(output_tail: Option<&str>) -> Option<String> {
    let output_tail = output_tail?;
    extract_output_excerpt_from_tail(output_tail, is_antigravity_output_noise)
}

fn is_gemini_shortcut_footer(lower: &str) -> bool {
    (lower.contains("accept edits") || lower.contains("show diff")) && lower.contains("tab")
}

fn is_antigravity_output_noise(raw: &str, normalized: &str) -> bool {
    let lower = normalized.to_ascii_lowercase();
    let compact = lower.trim_start_matches(|c: char| !c.is_ascii_alphanumeric()).trim_start();

    is_common_output_noise(raw, normalized)
        || raw.trim_start().starts_with('>')
        || lower.contains("antigravity cli")
        || lower.contains("antigravity starter quota")
        || extract_antigravity_footer_model(normalized).is_some()
        || lower.contains("? for shortcuts")
        || lower.contains("esc to cancel")
        || is_antigravity_spinner_line(raw)
        || compact.starts_with("tip:")
        || compact.starts_with("bash(")
        || compact.starts_with("schedule(")
        || compact == "command"
        || compact.starts_with("requesting permission for:")
        || compact.starts_with("do you want to proceed")
        || is_antigravity_permission_option_line(compact)
        || compact.contains("navigate") && compact.contains("edit command")
}

fn is_antigravity_permission_option_line(compact: &str) -> bool {
    let Some((number, label)) = compact.split_once('.') else {
        return false;
    };
    let label = label.trim_start();

    matches!(number.trim(), "1" | "2" | "3" | "4")
        && (label.starts_with("yes") || label.starts_with("no"))
}

fn extract_auggie_output_excerpt(output_tail: Option<&str>) -> Option<String> {
    let output_tail = output_tail?;
    extract_output_excerpt_from_tail(output_tail, |raw, normalized| {
        let lower = normalized.to_ascii_lowercase();
        is_common_output_noise(raw, normalized)
            || looks_like_auggie_output(raw)
            || lower.starts_with("version ")
            || lower.starts_with("new:")
            || lower.starts_with("you can ask questions")
            || lower.starts_with("use ctrl + enter")
            || lower.starts_with("use vim mode")
            || lower.starts_with("indexing disabled.")
            || lower.starts_with("to get the most out of auggie")
            || lower.starts_with("using model:")
            || lower.starts_with("message will be queued and run after current task")
            || lower.starts_with("executing tools...")
            || lower.starts_with("terminal - ")
            || lower == "command completed"
            || lower.starts_with("save model setting to")
            || lower.starts_with("save setting to")
            || lower.starts_with("local settings")
            || lower.starts_with("project settings")
            || lower.starts_with("user settings")
            || lower.starts_with("[insert]")
            || lower.starts_with("[↑↓] navigate")
            || lower.starts_with("select model")
            || raw.contains('▇')
            || raw.contains("$$")
            || auggie_footer_model_pattern().is_match(raw)
    })
}

fn extract_grok_output_excerpt(output_tail: Option<&str>) -> Option<String> {
    let output_tail = output_tail?;
    extract_latest_grok_reply_excerpt(output_tail)
        .or_else(|| extract_latest_grok_completion_excerpt(output_tail))
}

fn extract_latest_grok_reply_excerpt(output_tail: &str) -> Option<String> {
    let output_tail = strip_grok_right_aligned_timestamps(output_tail);
    extract_output_excerpt_from_tail(&output_tail, |raw, normalized| {
        is_grok_output_noise(raw, normalized) || is_grok_turn_completed_line(normalized)
    })
}

fn strip_grok_right_aligned_timestamps(output_tail: &str) -> String {
    output_tail
        .lines()
        .map(|line| grok_right_aligned_timestamp_pattern().replace(line, "").into_owned())
        .collect::<Vec<_>>()
        .join("\n")
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
    let trimmed = line.trim_matches(|c: char| is_box_chrome_char(c) || c.is_whitespace());
    let trimmed = trim_leading_output_marker(trimmed).unwrap_or(trimmed);

    trimmed.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn trim_leading_output_marker(line: &str) -> Option<&str> {
    let mut chars = line.chars();
    let marker = chars.next()?;
    let remainder = chars.as_str();
    if !matches!(marker, '✦' | '●' | '◊' | '•' | 'ℹ' | '~' | '⎿')
        || !remainder.starts_with(char::is_whitespace)
    {
        return None;
    }

    Some(remainder.trim_start())
}

fn is_box_chrome_char(c: char) -> bool {
    matches!(
        c,
        '│' | '┃'
            | '╭'
            | '╮'
            | '╯'
            | '╰'
            | '╔'
            | '╗'
            | '╝'
            | '║'
            | '╹'
            | '╻'
            | '┆'
            | '┊'
            | '─'
            | '═'
            | '█'
            | '▇'
            | '▌'
            | '▐'
            | '▕'
            | '▎'
            | '▍'
            | '▉'
            | '▀'
            | '▄'
            | '▗'
            | '▟'
            | '▜'
            | '▝'
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

fn output_fingerprint_for_kind(kind: AgentKind, output_tail: &str) -> Option<u64> {
    match kind {
        AgentKind::Amp => amp_output_fingerprint(output_tail),
        _ => full_output_fingerprint(output_tail),
    }
}

fn full_output_fingerprint(output_tail: &str) -> Option<u64> {
    fingerprint_signal(output_tail)
}

fn amp_output_fingerprint(output_tail: &str) -> Option<u64> {
    let lines: Vec<_> = output_tail.lines().collect();
    let start = lines.len().saturating_sub(AMP_STATUS_SIGNAL_WINDOW_LINES);
    let mut signal = Vec::new();

    for raw in &lines[start..] {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        let normalized = normalize_output_line(trimmed);
        if normalized.is_empty() || is_amp_output_noise(trimmed, &normalized) {
            continue;
        }

        signal.push(clamp_signal_head(&normalized, AMP_STATUS_SIGNAL_MAX_CHARS));
    }

    if signal.is_empty() {
        return None;
    }

    fingerprint_signal(&signal.join("\n"))
}

fn fingerprint_signal(signal: &str) -> Option<u64> {
    if signal.trim().is_empty() {
        return None;
    }

    let mut hasher = DefaultHasher::new();
    signal.hash(&mut hasher);
    Some(hasher.finish())
}

fn clamp_signal_head(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn looks_like_waiting_prompt(recent_lines: &[&str], output_tail: &str) -> bool {
    looks_like_codex_bottom_prompt(recent_lines)
        || looks_like_amp_home_screen(recent_lines, output_tail)
        || looks_like_claude_setup_screen(output_tail)
        || looks_like_claude_prompt(recent_lines, output_tail)
        || looks_like_opencode_home_screen(recent_lines, output_tail)
        || looks_like_pi_prompt(recent_lines, output_tail)
        || looks_like_gemini_prompt(recent_lines, output_tail)
        || looks_like_auggie_prompt(recent_lines, output_tail)
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
        amp_mode_label_in_text(&lower) && lower.contains("skills")
    });
    let has_workspace_footer =
        recent_lines.iter().any(|line| line.contains("~/") && line.contains('('));
    let has_amp_chrome = output_tail.contains("Welcome to")
        || output_tail.contains('╭')
        || output_tail.contains('╰');

    has_mode_skills && has_workspace_footer && has_amp_chrome
}

fn amp_mode_label_in_text(lower: &str) -> bool {
    lower.contains("smart") || lower.contains("rush") || lower.contains("deep")
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

fn looks_like_claude_output(output_tail: &str) -> bool {
    let lower = output_tail.to_ascii_lowercase();
    if lower.contains("welcome to claude code") || lower.contains("claude code v") {
        return true;
    }

    let recent_lines = recent_nonempty_lines(output_tail, PROMPT_LINE_WINDOW);
    looks_like_claude_prompt(&recent_lines, output_tail)
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

fn looks_like_gemini_prompt(recent_lines: &[&str], output_tail: &str) -> bool {
    let lower = output_tail.to_ascii_lowercase();
    let has_prompt = recent_lines.iter().any(|line| line.starts_with('>'));

    (has_prompt
        && recent_lines.iter().any(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("type your message") || lower.contains("/model")
        })
        && lower.contains("? for shortcuts"))
        || (lower.contains("action required") && lower.contains("allow execution of"))
}

fn looks_like_antigravity_prompt(recent_lines: &[&str], output_tail: &str) -> bool {
    if looks_like_antigravity_active(output_tail) {
        return false;
    }

    let has_prompt = recent_lines.iter().any(|line| line.trim_start().starts_with('>'));
    let has_shortcut_footer =
        recent_lines.iter().any(|line| line.to_ascii_lowercase().contains("? for shortcuts"));

    has_prompt && has_shortcut_footer && extract_antigravity_footer_model(output_tail).is_some()
}

fn looks_like_antigravity_active(output_tail: &str) -> bool {
    let recent_lines = recent_nonempty_lines(output_tail, PROMPT_LINE_WINDOW);
    let has_cancel_footer =
        recent_lines.iter().any(|line| line.to_ascii_lowercase().contains("esc to cancel"));

    has_cancel_footer && !looks_like_antigravity_permission_prompt(output_tail)
}

fn looks_like_antigravity_permission_prompt(output_tail: &str) -> bool {
    let lower = output_tail.to_ascii_lowercase();
    lower.contains("requesting permission for:") && lower.contains("do you want to proceed?")
}

fn is_antigravity_spinner_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.chars().next().is_some_and(is_braille_pattern) && trimmed.contains("...")
}

fn is_braille_pattern(c: char) -> bool {
    ('\u{2800}'..='\u{28ff}').contains(&c)
}

fn looks_like_auggie_prompt(recent_lines: &[&str], output_tail: &str) -> bool {
    let lower = output_tail.to_ascii_lowercase();

    (recent_lines.iter().any(|line| is_prompt_line(line))
        && recent_lines.iter().any(|line| lower_is_auggie_footer(line)))
        || (lower.contains("select model") && lower.contains("[esc] cancel"))
        || (lower.contains("save model setting to") && lower.contains("[enter] select"))
        || lower.contains("message will be queued and run after current task")
}

fn looks_like_grok_prompt(recent_lines: &[&str], output_tail: &str) -> bool {
    let has_recent_prompt = recent_lines.iter().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("│ ❯") || trimmed == "❯" || trimmed.starts_with("❯ ")
    });
    let has_recent_grok_chrome =
        recent_lines.iter().any(|line| line.to_ascii_lowercase().contains("grok build"));
    let has_recent_footer_hint = recent_lines.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("shift+tab:mode") || lower.contains("ctrl+.:shortcuts")
    });
    let has_launch_chooser = recent_lines
        .iter()
        .any(|line| line.to_ascii_lowercase().contains("grok build beta"))
        && recent_lines.iter().any(|line| line.to_ascii_lowercase().contains("try out grok build"));

    has_launch_chooser
        || has_recent_grok_chrome
            && (has_recent_prompt || has_recent_footer_hint)
            && latest_grok_prompt_chrome_has_no_newer_output(output_tail)
}

fn looks_like_grok_output(output_tail: &str) -> bool {
    let lower = output_tail.to_ascii_lowercase();
    lower.contains("grok build")
        || lower.contains("try out grok build")
        || output_tail.lines().any(|line| {
            let trimmed = line.trim();
            let normalized = normalize_output_line(trimmed);
            grok_model_pattern().is_match(trimmed)
                || extract_grok_footer_model(trimmed, &normalized).is_some()
        })
}

fn classify_grok_output_tail(output_tail: &str) -> Option<SessionStatus> {
    let recent_lines = recent_nonempty_lines(output_tail, PROMPT_LINE_WINDOW);
    (looks_like_grok_prompt(&recent_lines, output_tail)
        || extract_latest_grok_completion_excerpt(output_tail).is_some())
    .then_some(SessionStatus::WaitingInput)
}

fn looks_like_grok_active_waiting_footer(output_tail: &str) -> bool {
    let mut saw_waiting_footer = false;
    let mut meaningful_after_footer = false;

    for raw in output_tail.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        let normalized = normalize_output_line(trimmed);
        if normalized.is_empty() {
            continue;
        }

        if is_grok_active_waiting_footer_line(&normalized) {
            saw_waiting_footer = true;
            meaningful_after_footer = false;
            continue;
        }

        if saw_waiting_footer
            && !is_grok_output_noise(trimmed, &normalized)
            && !is_grok_prompt_chrome_line(trimmed, &normalized)
        {
            meaningful_after_footer = true;
        }
    }

    saw_waiting_footer && !meaningful_after_footer
}

fn is_grok_active_waiting_footer_line(normalized: &str) -> bool {
    let lower = normalized.to_ascii_lowercase();
    lower.contains("waiting…") || lower.contains("waiting...")
}

fn extract_latest_grok_completion_excerpt(output_tail: &str) -> Option<String> {
    let mut latest_completion: Option<String> = None;
    let mut meaningful_after_completion = false;

    for raw in output_tail.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        let normalized = normalize_output_line(trimmed);
        if normalized.is_empty() {
            continue;
        }

        if is_grok_turn_completed_line(&normalized) {
            latest_completion = Some(normalized);
            meaningful_after_completion = false;
            continue;
        }

        if latest_completion.is_some() && !is_grok_output_noise(trimmed, &normalized) {
            meaningful_after_completion = true;
        }
    }

    if meaningful_after_completion {
        return None;
    }

    latest_completion.map(|completion| clamp_excerpt_tail(&completion, OUTPUT_EXCERPT_MAX_CHARS))
}

fn is_grok_output_noise(raw: &str, normalized: &str) -> bool {
    let lower = normalized.to_ascii_lowercase();
    is_common_output_noise(raw, normalized)
        || normalized.starts_with('❯')
        || lower.contains("shift+tab:mode")
        || lower.contains("ctrl+.:shortcuts")
        || lower.contains("enter:send")
        || raw
            .trim()
            .chars()
            .all(|c| is_box_chrome_char(c) || c.is_ascii_digit() || c.is_whitespace())
        || raw.trim_start().starts_with('╭')
        || raw.trim_start().starts_with('╰')
        || raw.contains("│ ❯")
        || lower.contains("grok build")
        || lower.contains("try out grok build")
}

fn is_grok_turn_completed_line(normalized: &str) -> bool {
    normalized.to_ascii_lowercase().starts_with("turn completed in ")
}

fn latest_grok_prompt_chrome_has_no_newer_output(output_tail: &str) -> bool {
    let mut saw_prompt_chrome = false;
    let mut meaningful_after_prompt = false;

    for raw in output_tail.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        let normalized = normalize_output_line(trimmed);
        if normalized.is_empty() {
            continue;
        }

        if is_grok_prompt_chrome_line(trimmed, &normalized) {
            saw_prompt_chrome = true;
            meaningful_after_prompt = false;
            continue;
        }

        if saw_prompt_chrome
            && !is_grok_output_noise(trimmed, &normalized)
            && !is_grok_turn_completed_line(&normalized)
        {
            meaningful_after_prompt = true;
        }
    }

    saw_prompt_chrome && !meaningful_after_prompt
}

fn is_grok_prompt_chrome_line(raw: &str, normalized: &str) -> bool {
    let lower = normalized.to_ascii_lowercase();
    raw.contains("│ ❯")
        || normalized.starts_with('❯')
        || lower.contains("grok build")
        || lower.contains("shift+tab:mode")
        || lower.contains("ctrl+.:shortcuts")
        || lower.contains("enter:send")
}

fn lower_is_auggie_footer(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("[insert]") && lower.contains('?') && lower.contains('[') && lower.contains(']')
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
            r"(?i)(waiting for input|press enter|continue\?|confirm|(?:^|[^[:alnum:]-])approve(?:$|[^[:alnum:]-])|y/n|select an option)",
        )
        .expect("waiting regex should compile")
    })
}

fn codex_card_model_pattern() -> &'static Regex {
    static CODEX_CARD_MODEL: OnceLock<Regex> = OnceLock::new();
    CODEX_CARD_MODEL.get_or_init(|| {
        Regex::new(r"(?im)model:\s+(?P<model>gpt-.+?)\s+/model to change\b")
            .expect("codex card model regex should compile")
    })
}

fn codex_status_model_pattern() -> &'static Regex {
    static CODEX_STATUS_MODEL: OnceLock<Regex> = OnceLock::new();
    CODEX_STATUS_MODEL.get_or_init(|| {
        Regex::new(r"(?i)(?P<model>gpt-[^·│/\r\n]+)")
            .expect("codex status model regex should compile")
    })
}

fn amp_mode_pattern() -> &'static Regex {
    static AMP_MODE: OnceLock<Regex> = OnceLock::new();
    AMP_MODE.get_or_init(|| {
        Regex::new(r"(?i)(?P<mode>deep²|deep2|smart|rush|deep)(?:\b|\s|─)")
            .expect("amp mode regex should compile")
    })
}

fn amp_compact_mode_pattern() -> &'static Regex {
    static AMP_COMPACT_MODE: OnceLock<Regex> = OnceLock::new();
    AMP_COMPACT_MODE.get_or_init(|| {
        Regex::new(r"(?i)(?P<bolt>↯\s*)?(?P<mode>deep²|deep2|smart|rush|deep)(?:\b|\s|─)")
            .expect("amp compact mode regex should compile")
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

fn gemini_footer_model_pattern() -> &'static Regex {
    static GEMINI_FOOTER_MODEL: OnceLock<Regex> = OnceLock::new();
    GEMINI_FOOTER_MODEL.get_or_init(|| {
        Regex::new(r"(?i)/model\s+(?P<model>.+?)\s*$")
            .expect("gemini footer model regex should compile")
    })
}

fn gemini_table_footer_model_pattern() -> &'static Regex {
    static GEMINI_TABLE_FOOTER_MODEL: OnceLock<Regex> = OnceLock::new();
    GEMINI_TABLE_FOOTER_MODEL.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:no\s+sandbox|sandbox)\s+(?P<model>[A-Za-z0-9][A-Za-z0-9._-]*(?:\s+\([^)]+\))?)\s*(?:…|\.\.\.)?\s*$",
        )
        .expect("gemini table footer model regex should compile")
    })
}

fn antigravity_footer_model_pattern() -> &'static Regex {
    static ANTIGRAVITY_FOOTER_MODEL: OnceLock<Regex> = OnceLock::new();
    ANTIGRAVITY_FOOTER_MODEL.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?P<model>(?:Gemini|Claude|GPT|Grok|OpenAI|Anthropic)[A-Za-z0-9 ._-]*\([^)]+\))\s*$",
        )
        .expect("antigravity footer model regex should compile")
    })
}

fn auggie_using_model_pattern() -> &'static Regex {
    static AUGGIE_USING_MODEL: OnceLock<Regex> = OnceLock::new();
    AUGGIE_USING_MODEL.get_or_init(|| {
        Regex::new(r"(?i)\busing model:\s*(?P<model>.+?)\s*$")
            .expect("auggie using model regex should compile")
    })
}

fn auggie_footer_model_pattern() -> &'static Regex {
    static AUGGIE_FOOTER_MODEL: OnceLock<Regex> = OnceLock::new();
    AUGGIE_FOOTER_MODEL.get_or_init(|| {
        Regex::new(r"\[(?P<model>[^\]]+)\]\s+~(?:\s*$|[/\w.-].*$)")
            .expect("auggie footer model regex should compile")
    })
}

fn auggie_selected_model_pattern() -> &'static Regex {
    static AUGGIE_SELECTED_MODEL: OnceLock<Regex> = OnceLock::new();
    AUGGIE_SELECTED_MODEL.get_or_init(|| {
        Regex::new(r"^(?:●|•)?\s*(?P<model>.+?)(?:\s+\([^)]+\))*\s+\$+\s*$")
            .expect("auggie selected model regex should compile")
    })
}

fn grok_model_pattern() -> &'static Regex {
    static GROK_MODEL: OnceLock<Regex> = OnceLock::new();
    GROK_MODEL.get_or_init(|| {
        Regex::new(r"(?i)\b(?P<model>grok\s+[0-9]+(?:\.[0-9]+)*)\b")
            .expect("grok model regex should compile")
    })
}

fn grok_footer_model_pattern() -> &'static Regex {
    static GROK_FOOTER_MODEL: OnceLock<Regex> = OnceLock::new();
    GROK_FOOTER_MODEL.get_or_init(|| {
        Regex::new(r"(?i)\b(?P<model>composer\s+[0-9]+(?:\.[0-9]+)*)\b\s*·")
            .expect("grok footer model regex should compile")
    })
}

fn grok_right_aligned_timestamp_pattern() -> &'static Regex {
    static GROK_RIGHT_ALIGNED_TIMESTAMP: OnceLock<Regex> = OnceLock::new();
    GROK_RIGHT_ALIGNED_TIMESTAMP.get_or_init(|| {
        Regex::new(r"(?i)\s{2,}\d{1,2}:\d{2}\s(?:AM|PM)\s*$")
            .expect("grok right-aligned timestamp regex should compile")
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

fn extract_auggie_using_model(output_tail: &str) -> Option<String> {
    output_tail.lines().rev().find_map(|line| {
        auggie_using_model_pattern()
            .captures(line.trim())
            .and_then(|captures| captures.name("model"))
            .map(|matched| normalize_detail_label(matched.as_str()))
            .filter(|label| !label.is_empty())
    })
}

fn extract_auggie_footer_model(output_tail: &str) -> Option<String> {
    output_tail.lines().rev().find_map(|line| {
        auggie_footer_model_pattern()
            .captures(line.trim())
            .and_then(|captures| captures.name("model"))
            .map(|matched| normalize_detail_label(matched.as_str()))
            .filter(|label| !label.is_empty())
    })
}

fn extract_auggie_selected_model(output_tail: &str) -> Option<String> {
    output_tail.lines().find_map(|line| {
        let normalized = normalize_output_line(line.trim());
        auggie_selected_model_pattern()
            .captures(normalized.as_str())
            .and_then(|captures| captures.name("model"))
            .map(|matched| normalize_detail_label(matched.as_str()))
            .filter(|label| !label.is_empty())
    })
}

fn normalize_detail_label(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        amp_output_fingerprint, classify_antigravity_output_tail, classify_grok_output_tail,
        classify_output_tail, extract_amp_detail, extract_amp_output_excerpt,
        extract_antigravity_detail, extract_antigravity_output_excerpt, extract_auggie_detail,
        extract_auggie_output_excerpt, extract_claude_detail, extract_claude_output_excerpt,
        extract_codex_detail, extract_codex_output_excerpt, extract_gemini_detail,
        extract_gemini_output_excerpt, extract_grok_detail, extract_grok_output_excerpt,
        extract_opencode_detail, extract_opencode_output_excerpt, extract_pi_detail,
        extract_pi_output_excerpt, AdapterRegistry, AgentAdapter, SessionTracker,
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
        let antigravity = snapshot("%9", "agy", false);

        assert_eq!(registry.detect_kind(&codex, None), Some(AgentKind::Codex));
        assert_eq!(registry.detect_kind(&codex_variant, None), Some(AgentKind::Codex));
        assert_eq!(registry.detect_kind(&amp, None), Some(AgentKind::Amp));
        assert_eq!(registry.detect_kind(&claude, None), Some(AgentKind::ClaudeCode));
        assert_eq!(registry.detect_kind(&claude_title, None), Some(AgentKind::ClaudeCode));
        assert_eq!(registry.detect_kind(&claude_code, None), Some(AgentKind::ClaudeCode));
        // wrapped binary (e.g. nix .claude-unwrapped) and rewritten task title (glyph survives)
        let claude_wrapped = snapshot("%24", ".claude-unwrapped", false);
        let claude_task_title =
            snapshot_with_title("%25", "node", false, "✳ Plan and start MIR-1094 using Linear MCP");
        let claude_braille_title =
            snapshot_with_title("%26", "node", false, "⠐ Plan and start MIR-1094 using Linear MCP");
        assert_eq!(registry.detect_kind(&claude_wrapped, None), Some(AgentKind::ClaudeCode));
        assert_eq!(registry.detect_kind(&claude_task_title, None), Some(AgentKind::ClaudeCode));
        assert_eq!(registry.detect_kind(&claude_braille_title, None), None);
        assert_eq!(registry.detect_kind(&opencode, None), Some(AgentKind::OpenCode));
        assert_eq!(registry.detect_kind(&opencode_title, None), Some(AgentKind::OpenCode));
        assert_eq!(registry.detect_kind(&pi, None), Some(AgentKind::Pi));
        assert_eq!(registry.detect_kind(&pi_title, None), Some(AgentKind::Pi));
        assert_eq!(registry.detect_kind(&pi_agent, None), Some(AgentKind::Pi));
        assert_eq!(registry.detect_kind(&antigravity, None), Some(AgentKind::AntigravityCli));
    }

    #[test]
    fn registry_keeps_planned_adapter_scaffolds_disabled_by_default() {
        let registry = AdapterRegistry::v1();

        assert_eq!(registry.enabled_kinds(), AgentKind::enabled_kinds().collect::<Vec<_>>());
        for (kind, command) in planned_command_fixtures() {
            let pane = snapshot("%40", command, false);

            assert!(!kind.is_enabled(), "{kind:?} should stay disabled until its issue is done");
            assert_eq!(
                registry.detect_kind(&pane, None),
                None,
                "planned scaffold should not be active yet: {kind:?} / {command}"
            );
        }
    }

    #[test]
    fn planned_adapter_scaffolds_detect_primary_commands_when_all_adapters_are_loaded() {
        let registry = AdapterRegistry::all_for_tests();

        for (kind, command) in planned_command_fixtures() {
            let pane = snapshot("%44", command, false);

            assert_eq!(
                registry.detect_kind(&pane, None),
                Some(kind),
                "planned scaffold should be ready for {kind:?}: {command}"
            );
        }
    }

    #[test]
    fn registry_detects_process_title_fixtures() {
        let registry = AdapterRegistry::v1();

        for (kind, command) in process_title_fixtures() {
            let pane = snapshot("%41", command, false);

            assert_eq!(
                registry.detect_kind(&pane, None),
                Some(kind),
                "command fixture should detect {kind:?}: {command}"
            );
        }
    }

    #[test]
    fn registry_detects_pane_title_fixtures() {
        let registry = AdapterRegistry::v1();

        for (kind, command, title) in pane_title_fixtures() {
            let pane = snapshot_with_title("%42", command, false, title);

            assert_eq!(
                registry.detect_kind(&pane, None),
                Some(kind),
                "title fixture should detect {kind:?}: {command} / {title}"
            );
        }
    }

    #[test]
    fn tracker_detects_output_snippet_fixtures() {
        for (kind, command, title, output_tail) in output_snippet_fixtures() {
            let mut tracker = SessionTracker::new();
            let pane = snapshot_with_title("%43", command, false, title);
            let output_tails = HashMap::from([(pane.pane_id.clone(), output_tail.to_string())]);

            let records = tracker.refresh(&[pane], &output_tails, Instant::now());

            assert_eq!(records.len(), 1, "output fixture should produce one record for {kind:?}");
            assert_eq!(records[0].kind, kind, "output fixture should detect {kind:?}");
        }
    }

    #[test]
    fn registry_captures_output_for_node_wrapped_agent_candidates() {
        let registry = AdapterRegistry::v1();
        let gemini = snapshot_with_title("%24", "node", false, "◇  Ready (workspace)");

        assert!(registry.needs_output_tail(&gemini, None, None));
        assert!(registry.needs_output_tail(
            &snapshot_with_title("%25", "node", false, "Implement feature"),
            None,
            Some(AgentKind::Auggie),
        ));
    }

    #[test]
    fn registry_captures_output_for_claude_braille_confirmation_candidates() {
        let registry = AdapterRegistry::v1();
        let claude_braille =
            snapshot_with_title("%25", "2.1.76", false, "⠐ Plan and start MIR-1094");

        assert!(registry.needs_output_tail(&claude_braille, None, None));
    }

    #[test]
    fn tracker_detects_gemini_and_auggie_from_live_output() {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let gemini = snapshot_with_title("%24", "node", false, "◇  Ready (workspace)");
        let auggie = snapshot_with_title("%25", "node", false, "auggie");
        let output_tails = HashMap::from([
            (
                gemini.pane_id.clone(),
                "\
✦ Hello. I'm Gemini CLI, your senior software engineering assistant. How can I help you today?

? for shortcuts
>   Type your message or @path/to/file
~                             no sandbox (see /docs)                              /model Auto (Gemini 3)
"
                .to_string(),
            ),
            (
                auggie.pane_id.clone(),
                "\
› hello

● Hi

  Hello! What would you like help with?

◊ Using model: GPT-5.4
──────────────────────────────────────────────────────────────────────────────────────────────────────────
›
──────────────────────────────────────────────────────────────────────────────────────────────────────────
 [INSERT] ? to show shortcuts                                                                [GPT-5.4]  ~
"
                .to_string(),
            ),
        ]);

        let records = tracker.refresh(&[gemini, auggie], &output_tails, now);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].kind, AgentKind::GeminiCli);
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
        assert_eq!(records[1].kind, AgentKind::Auggie);
        assert_eq!(records[1].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn tracker_detects_antigravity_from_live_output() {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let pane = snapshot_with_title("%27", "node", false, "worker");
        let output_tails = HashMap::from([(
            pane.pane_id.clone(),
            "\
Antigravity CLI 1.0.10
Gemini 3.5 Flash (Medium)

────────────────────────────────────────────────────────────
> Say exactly: hello

  hello

────────────────────────────────────────────────────────────────────────────────
>
────────────────────────────────────────────────────────────────────────────────
? for shortcuts                                           Gemini 3.5 Flash (Medium)
"
            .to_string(),
        )]);

        let records = tracker.refresh(&[pane], &output_tails, now);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::AntigravityCli);
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
        assert_eq!(records[0].output_excerpt.as_deref(), Some("hello"));
        assert_eq!(
            records[0].detail,
            Some(
                AgentDetail {
                    label: "Gemini 3.5 Flash (Medium)".to_string(),
                    tone: AgentDetailTone::Neutral,
                }
                .into()
            )
        );
    }

    #[test]
    fn tracker_detects_process_identified_node_wrapped_auggie_without_title() {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let auggie = snapshot_with_title("%25", "node", false, "Implement Bevy");
        let process_kinds = HashMap::from([(auggie.pane_id.clone(), AgentKind::Auggie)]);
        let output_tails = HashMap::from([(
            auggie.pane_id.clone(),
            "\
─────────────────────────────────────────────────────
› Message will be queued and run after current task • Press ↑ to edit queue
─────────────────────────────────────────────────────
 ? to show shortcuts                      [Opus 4.8]
                                           ~/work/project
"
            .to_string(),
        )]);

        let records =
            tracker.refresh_with_process_kinds(&[auggie], &output_tails, &process_kinds, now);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::Auggie);
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn tracker_detects_claude_braille_title_only_with_claude_output() {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let pane =
            snapshot_with_title("%27", "node", false, "⠐ Plan and start MIR-1094 using Linear MCP");
        let output_tails = HashMap::from([(
            pane.pane_id.clone(),
            "\
╭─── Claude Code v2.1.76 ──────────────────────────────────────────────╮
│      Sonnet 4.6 · Claude Pro · example.com's Organization          │
╰───────────────────────────────────────────────────────────────────────╯

❯
  ? for shortcuts
  ◐ medium · /effort
"
            .to_string(),
        )]);

        let records = tracker.refresh(&[pane], &output_tails, now);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::ClaudeCode);
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn tracker_does_not_detect_claude_from_generic_node_output_mentions() {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let pane = snapshot_with_title("%28", "node", false, "worker");
        let output_tails = HashMap::from([(
            pane.pane_id.clone(),
            "Release notes mention Claude Code but this pane is not Claude.".to_string(),
        )]);

        let records = tracker.refresh(&[pane], &output_tails, now);

        assert!(records.is_empty());
    }

    #[test]
    fn tracker_keeps_previous_claude_from_braille_spinner_title() {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let running_pane = snapshot("%29", "claude", false);

        tracker.refresh(&[running_pane], &HashMap::new(), now);

        let spinner_pane =
            snapshot_with_title("%29", "node", false, "⠐ Plan and start MIR-1094 using Linear MCP");
        let records =
            tracker.refresh(&[spinner_pane], &HashMap::new(), now + Duration::from_secs(5));

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::ClaudeCode);
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn tracker_does_not_keep_previous_claude_when_command_matches_another_agent() {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let running_pane = snapshot("%30", "claude", false);

        tracker.refresh(&[running_pane], &HashMap::new(), now);

        let codex_pane = snapshot_with_title(
            "%30",
            "codex",
            false,
            "⠐ Plan and start MIR-1094 using Linear MCP",
        );
        let records = tracker.refresh(&[codex_pane], &HashMap::new(), now + Duration::from_secs(5));

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::Codex);
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn registry_does_not_let_claude_direct_title_override_explicit_non_claude_command() {
        let registry = AdapterRegistry::v1();
        let opencode_with_stale_claude_title =
            snapshot_with_title("%31", "opencode", false, "✳ Plan and start MIR-1094");
        let gemini_with_stale_claude_title =
            snapshot_with_title("%32", "gemini", false, "Claude Code");

        assert_eq!(
            registry.detect_kind(&opencode_with_stale_claude_title, None),
            Some(AgentKind::OpenCode)
        );
        assert_eq!(
            registry.detect_kind(&gemini_with_stale_claude_title, None),
            Some(AgentKind::GeminiCli)
        );
    }

    #[test]
    fn tracker_does_not_keep_previous_claude_from_stale_direct_title_when_command_matches_another_agent(
    ) {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let running_pane = snapshot("%32", "claude", false);

        tracker.refresh(&[running_pane], &HashMap::new(), now);

        let opencode_pane =
            snapshot_with_title("%32", "opencode", false, "✳ Plan and start MIR-1094");
        let records =
            tracker.refresh(&[opencode_pane], &HashMap::new(), now + Duration::from_secs(5));

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::OpenCode);
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn tracker_does_not_keep_previous_claude_from_stale_output_when_command_matches_another_agent()
    {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let running_pane = snapshot("%31", "claude", false);

        tracker.refresh(&[running_pane], &HashMap::new(), now);

        let codex_pane = snapshot_with_title(
            "%31",
            "codex",
            false,
            "⠐ Plan and start MIR-1094 using Linear MCP",
        );
        let output_tails = HashMap::from([(
            codex_pane.pane_id.clone(),
            "\
╭─── Claude Code v2.1.76 ──────────────────────────────────────────────╮
│      Sonnet 4.6 · Claude Pro · example.com's Organization          │
╰───────────────────────────────────────────────────────────────────────╯

❯
  ? for shortcuts
  ◐ medium · /effort
"
            .to_string(),
        )]);
        let records = tracker.refresh(&[codex_pane], &output_tails, now + Duration::from_secs(5));

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::Codex);
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
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
gpt-5.4 xhigh fast \u{00b7} 40% left \u{00b7} ~/workspace\n";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn codex_working_banner_still_looks_like_waiting_prompt_on_first_load() {
        let output_tail = "\
• Working (2m 45s • esc to interrupt)

› Explain this codebase

gpt-5.4 xhigh fast · 20% left · ~/workspace
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
~/workspace (main)\n\
MCP 2 failed\n";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn amp_rush_home_screen_marks_waiting_input() {
        let output_tail = "\
Welcome to
Ctrl+O for

rush 4 skills
~/workspace (main)
MCP 2 failed
";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn amp_deep_home_screen_marks_waiting_input() {
        let output_tail = "\
Welcome to
Ctrl+O for

deep² 4 skills
~/workspace (main)
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
╰──────────────────────────────────────────────────────────────────────────────────~/workspace (main)─╯
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
│      Sonnet 4.6 · Claude Pro · example.com's Organization          │
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
~/workspace:main             ctrl+t variants  tab agents  ctrl+p co1.2.26
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
    fn gemini_prompt_marks_waiting_input() {
        let output_tail = "\
? for shortcuts
>   Type your message or @path/to/file
~                             no sandbox (see /docs)                              /model Auto (Gemini 3)
";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn gemini_action_prompt_marks_waiting_input() {
        let output_tail = "\
✦ I will wait for 5 seconds and then say hello.

Action Required
Allow execution of: 'sleep, echo'?

● 1. Allow once
  2. Allow for this session
  3. No, suggest changes (esc)
";

        assert_eq!(classify_output_tail(output_tail), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn antigravity_prompt_and_permission_mark_waiting_input() {
        let prompt = "\
────────────────────────────────────────────────────────────────────────────────
>
────────────────────────────────────────────────────────────────────────────────
? for shortcuts                                           Gemini 3.5 Flash (Medium)
";
        let permission = "\
● Bash(sleep 20) (ctrl+o to expand)

Command
────────────────────────────────────────────────────────────────────────────────

  Requesting permission for: sleep 20

Do you want to proceed?
> 1. Yes
  2. Yes, and always allow in this conversation for commands that start with 'sleep'
  3. Yes, and always allow for commands that start with 'sleep' (Persist to settings.json)
  4. No

  ↑/↓ Navigate · tab Amend · e edit command
esc to cancel                                            Gemini 3.5 Flash (Medium)
";

        assert_eq!(classify_antigravity_output_tail(prompt), Some(SessionStatus::WaitingInput));
        assert_eq!(classify_antigravity_output_tail(permission), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn antigravity_active_generation_marks_running() {
        let loading = "\
> Write a 100-line poem. Start immediately and do not use tools.
⣷  Loading...
────────────────────────────────────────────────────────────────────────────────
>
────────────────────────────────────────────────────────────────────────────────
esc to cancel                                            Gemini 3.5 Flash (Medium)
";
        let streaming = "\
> Write a 100-line poem. Start immediately and do not use tools.
⣻  Crafting the Poem's Structure...
└ Tip: Press ? to see keyboard shortcuts.
────────────────────────────────────────────────────────────────────────────────
>
────────────────────────────────────────────────────────────────────────────────
esc to cancel                                            Gemini 3.5 Flash (Medium)
";
        let streaming_after_status_scrolls = "\
  The mountains stand as guardians of the sky,
  Watching the endless ages passing by,
  Their stony peaks are kissed by morning dew,
  Painted in shades of purple, gold, and blue.
────────────────────────────────────────────────────────────────────────────────
>
────────────────────────────────────────────────────────────────────────────────
esc to cancel                                            Gemini 3.5 Flash (Medium)
";

        assert_eq!(classify_antigravity_output_tail(loading), Some(SessionStatus::Running));
        assert_eq!(classify_antigravity_output_tail(streaming), Some(SessionStatus::Running));
        assert_eq!(
            classify_antigravity_output_tail(streaming_after_status_scrolls),
            Some(SessionStatus::Running)
        );
    }

    #[test]
    fn auggie_prompt_and_model_menu_mark_waiting_input() {
        let prompt = "\
›
[INSERT] ? to show shortcuts                                                                [GPT-5.4]  ~
";
        let model_menu = "\
Select model
● GPT-5.4 (default) (current)                          $$
[↑↓] Navigate • [Enter] Select • [/] Search • [Esc] Cancel
";

        assert_eq!(classify_output_tail(prompt), Some(SessionStatus::WaitingInput));
        assert_eq!(classify_output_tail(model_menu), Some(SessionStatus::WaitingInput));
    }

    #[test]
    fn pi_idle_screen_marks_waiting_input() {
        let output_tail = "\
pi v0.58.0
ctrl+l to select model
Warning: No models available. Use /login or set an API key environment variable.
~/workspace/specs/demo
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
~/workspace/specs/demo
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
~/workspace/specs/demo
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

~/workspace/specs/demo
↑2.2k ↓64 $0.013 (sub) 1.1%/200k (auto)                                            claude-haiku-4-5 • medium
"
            .to_string(),
        )]);
        let second_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
Hello! I'm ready to help.

Done! 🎉

~/workspace/specs/demo
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

        assert!(registry.needs_output_tail(&snapshot("%1", "codex", false), Some(&previous), None,));
        assert!(!registry.needs_output_tail(&snapshot("%1", "zsh", false), Some(&previous), None,));
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

gpt-5.4 xhigh fast · 40% left · ~/workspace
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

gpt-5.4 xhigh fast · 78% left · ~/workspace
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

gpt-5.4 xhigh fast · 78% left · ~/workspace
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

gpt-5.4 xhigh fast · 40% left · ~/workspace
"
            .to_string(),
        )]);
        let second_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
processing task chunk 2
› Write tests for @filename

gpt-5.4 xhigh fast · 39% left · ~/workspace
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

gpt-5.4 xhigh fast · 20% left · ~/workspace
"
            .to_string(),
        )]);
        let second_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
• Working (2m 50s • esc to interrupt)

› Explain this codebase

gpt-5.4 xhigh fast · 20% left · ~/workspace
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
gpt-5.4 xhigh fast · 100% left · ~/workspace
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
    fn codex_detail_extracts_from_weekly_footer_line() {
        let output_tail = "\
› Improve documentation in @filename

gpt-5.5 xhigh · weekly 39% left · Context 0% used
";

        assert_eq!(
            extract_codex_detail(Some(output_tail)),
            Some(AgentDetail {
                label: "gpt-5.5 xhigh".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn codex_detail_extracts_model_before_enabled_status_items() {
        let output_tail = "\
› Improve documentation in @filename

gpt-5.5 xhigh · 14K used · weekly 38% left · Context 2% used
";

        assert_eq!(
            extract_codex_detail(Some(output_tail)),
            Some(AgentDetail {
                label: "gpt-5.5 xhigh".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn codex_detail_extracts_model_after_enabled_status_items() {
        let output_tail = "\
› Improve documentation in @filename

weekly 39% left · approvals on · gpt-5.5 xhigh · Context 0% used
";

        assert_eq!(
            extract_codex_detail(Some(output_tail)),
            Some(AgentDetail {
                label: "gpt-5.5 xhigh".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn codex_detail_ignores_plain_output_mentions_of_gpt_models() {
        let output_tail = "\
The model name starts with gpt-5.5, but this is not a status bar.
";

        assert_eq!(extract_codex_detail(Some(output_tail)), None);
    }

    #[test]
    fn codex_output_excerpt_skips_prompt_and_footer() {
        let output_tail = "\
Here is the first part of the answer.
The tracker now keeps waiting panes stable.
› Write tests for @filename

gpt-5.4 xhigh fast · 40% left · ~/workspace
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
    fn gemini_detail_extracts_from_footer_line() {
        let output_tail = "\
~                             no sandbox (see /docs)                              /model Auto (Gemini 3)
";

        assert_eq!(
            extract_gemini_detail(Some(output_tail)),
            Some(AgentDetail {
                label: "Auto (Gemini 3)".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );

        let table_footer = "\
 workspace (/directory)   branch    sandbox      /model
 ~/workspace              main      no sandbox   gemini-3-flash-preview   …
";

        assert_eq!(
            extract_gemini_detail(Some(table_footer)),
            Some(AgentDetail {
                label: "gemini-3-flash-preview".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn antigravity_detail_extracts_from_header_or_footer_model() {
        let output_tail = "\
Antigravity CLI 1.0.10
Gemini 3.5 Flash (Medium)
";

        assert_eq!(
            extract_antigravity_detail(Some(output_tail)),
            Some(AgentDetail {
                label: "Gemini 3.5 Flash (Medium)".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );

        let footer = "\
? for shortcuts                                           Gemini 3.5 Flash (Medium)
";

        assert_eq!(
            extract_antigravity_detail(Some(footer)),
            Some(AgentDetail {
                label: "Gemini 3.5 Flash (Medium)".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn gemini_output_excerpt_skips_prompt_and_footer() {
        let output_tail = "\
✦ Hello. I'm Gemini CLI, your senior software engineering assistant. How can I help you today?

? for shortcuts
>   Type your message or @path/to/file
~                             no sandbox (see /docs)                              /model Auto (Gemini 3)
";

        assert_eq!(
            extract_gemini_output_excerpt(Some(output_tail)),
            Some(
                "...ni CLI, your senior software engineering assistant. How can I help you today?"
                    .to_string()
            )
        );
    }

    #[test]
    fn gemini_output_excerpt_ignores_shift_tab_footer_when_reply_is_out_of_tail() {
        let output_tail = "\
──────────────────────────────────────────────────────────────────────────────────────────────────────────
shift+tab to accept edits
▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
>   Type your message or @path/to/file
▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄
~                             no sandbox (see /docs)                              /model Auto (Gemini 3)
";

        assert_eq!(extract_gemini_output_excerpt(Some(output_tail)), None);

        let table_footer = "\
────────────────────────────────────────────────────────────────────────────
 Shift+Tab to accept edits

 >   Type your message or @path/to/file

 workspace (/directory)   branch    sandbox      /model
 ~/workspace              main      no sandbox   gemini-3-flash-preview   …
";

        assert_eq!(extract_gemini_output_excerpt(Some(table_footer)), None);
    }

    #[test]
    fn antigravity_output_excerpt_skips_prompt_status_and_footer() {
        let output_tail = "\
Antigravity CLI 1.0.10
Gemini 3.5 Flash (Medium)

────────────────────────────────────────────────────────────
> Write a 100-line poem. Start immediately and do not use tools.

  The morning opens silver at the gate,
  And every quiet window learns to wait.

────────────────────────────────────────────────────────────────────────────────
>
────────────────────────────────────────────────────────────────────────────────
? for shortcuts                                           Gemini 3.5 Flash (Medium)
";

        assert_eq!(
            extract_antigravity_output_excerpt(Some(output_tail)),
            Some(
                "The morning opens silver at the gate, And every quiet window learns to wait."
                    .to_string()
            )
        );

        let active_tail = "\
> Write a 100-line poem. Start immediately and do not use tools.
⣻  Crafting the Poem's Structure...
└ Tip: Press ? to see keyboard shortcuts.
────────────────────────────────────────────────────────────────────────────────
>
────────────────────────────────────────────────────────────────────────────────
esc to cancel                                            Gemini 3.5 Flash (Medium)
";

        assert_eq!(extract_antigravity_output_excerpt(Some(active_tail)), None);
    }

    #[test]
    fn auggie_detail_extracts_from_footer_and_model_menu() {
        assert_eq!(
            extract_auggie_detail(Some(
                "◊ Using model: GPT-5.4\n [INSERT] ? to show shortcuts                                                                [GPT-5.4]  ~"
            )),
            Some(AgentDetail {
                label: "GPT-5.4".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );

        assert_eq!(
            extract_auggie_detail(Some(
                "◊ Using model: Opus 4.8\n ? to show shortcuts                           [Opus 4.8]  ~/workspace"
            )),
            Some(AgentDetail {
                label: "Opus 4.8".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );

        assert_eq!(
            extract_auggie_detail(Some(
                "Select model\n● GPT-5.4 (default) (current)                          $$\n[↑↓] Navigate • [Enter] Select • [/] Search • [Esc] Cancel"
            )),
            Some(AgentDetail {
                label: "GPT-5.4".to_string(),
                tone: AgentDetailTone::Neutral,
            })
        );
    }

    #[test]
    fn auggie_output_excerpt_prefers_latest_reply_block() {
        let output_tail = "\
› hello

● Hi

  Hello! What would you like help with?

◊ Using model: GPT-5.4
──────────────────────────────────────────────────────────────────────────────────────────────────────────
›
──────────────────────────────────────────────────────────────────────────────────────────────────────────
 [INSERT] ? to show shortcuts                                                                [GPT-5.4]  ~
";

        assert_eq!(
            extract_auggie_output_excerpt(Some(output_tail)),
            Some("Hello! What would you like help with?".to_string())
        );
    }

    #[test]
    fn auggie_output_excerpt_keeps_delayed_reply_after_tool_completion() {
        let output_tail = "\
› say hello in 5 seconds

~ I see the user wants me to say hello in 5 seconds.

● Terminal - sleep 5
  ⎿ Command completed

● hello
──────────────────────────────────────────────────────────────────────────────────────────────────────────
›
──────────────────────────────────────────────────────────────────────────────────────────────────────────
 [INSERT] ? to show shortcuts                                                                [GPT-5.4]  ~
";

        assert_eq!(extract_auggie_output_excerpt(Some(output_tail)), Some("hello".to_string()));
    }

    #[test]
    fn amp_detail_extracts_modes_and_bolt_modifier() {
        let smart = "\
╭─────────────────────smart──30 skills─╮
╰────────────────~/workspace (main)─╯
";
        let rush = "\
╭──────────────────────rush──4 skills─╮
╰────────────────~/workspace (main)─╯
";
        let deep = "\
╭──────────────────────deep²──4 skills─╮
╰────────────────~/workspace (main)─╯
";
        let compact_smart = "\
╭─────────────────────────────────────────────────────────────────── smart ─╮
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
";
        let compact_deep = "\
╭─────────────────────────────────────────────────────────────────── deep² ─╮
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
";
        let compact_bolt_smart = "\
╭───────────────────────────────────────────────────────────────── ↯smart ─╮
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
";
        let compact_bolt_deep = "\
╭───────────────────────────────────────────────────────────────── ↯deep² ─╮
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
";
        let compact_rush = "\
╭─────────────────────────────────────────────────────────────────── ↯rush ─╮
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
";
        let compact_rush_without_marker = "\
╭──────────────────────────────────────────────────────────────────── rush ─╮
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
";

        assert_eq!(
            extract_amp_detail(Some(smart)),
            Some(AgentDetail { label: "smart".to_string(), tone: AgentDetailTone::AmpSmart })
        );
        assert_eq!(
            extract_amp_detail(Some(rush)),
            Some(AgentDetail { label: "rush".to_string(), tone: AgentDetailTone::AmpRush })
        );
        assert_eq!(
            extract_amp_detail(Some(deep)),
            Some(AgentDetail { label: "deep²".to_string(), tone: AgentDetailTone::AmpDeep })
        );
        assert_eq!(
            extract_amp_detail(Some(compact_smart)),
            Some(AgentDetail { label: "smart".to_string(), tone: AgentDetailTone::AmpSmart })
        );
        assert_eq!(
            extract_amp_detail(Some(compact_deep)),
            Some(AgentDetail { label: "deep²".to_string(), tone: AgentDetailTone::AmpDeep })
        );
        assert_eq!(
            extract_amp_detail(Some(compact_bolt_smart)),
            Some(AgentDetail { label: "↯smart".to_string(), tone: AgentDetailTone::AmpSmart })
        );
        assert_eq!(
            extract_amp_detail(Some(compact_bolt_deep)),
            Some(AgentDetail { label: "↯deep²".to_string(), tone: AgentDetailTone::AmpDeep })
        );
        assert_eq!(
            extract_amp_detail(Some(compact_rush)),
            Some(AgentDetail { label: "↯rush".to_string(), tone: AgentDetailTone::AmpRush })
        );
        assert_eq!(extract_amp_detail(Some(compact_rush_without_marker)), None);
    }

    #[test]
    fn amp_output_excerpt_skips_footer_chrome() {
        let output_tail = "\
  ┃ c

  ✓ Thinking ▶

  Could you clarify what you'd like me to help with?

╭─26% of 168k · $0.45───────────────────────────────────────────────────────────────────smart──30 skills─╮
╰──────────────────────────────────────────────────────────────────────────────────~/workspace (main)─╯
";

        assert_eq!(
            extract_amp_output_excerpt(Some(output_tail)),
            Some("Could you clarify what you'd like me to help with?".to_string())
        );
    }

    #[test]
    fn amp_output_excerpt_skips_compact_pricing_and_mode_border() {
        let output_tail = "\
 ┃ hello

 Hello!
╭────────────────────────────────────────────────────────── $0.001 ─ ↯rush ─╮
│                                                                           │
│                                                                           │
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
";

        assert_eq!(extract_amp_output_excerpt(Some(output_tail)), Some("Hello!".to_string()));
    }

    #[test]
    fn amp_fingerprint_ignores_compact_pricing_and_mode_border() {
        let first = "\
 ┃ hello

 Hello!
╭────────────────────────────────────────────────────────── $0.001 ─ ↯rush ─╮
│                                                                           │
│                                                                           │
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
";
        let second = "\
 ┃ hello

 Hello!
╭────────────────────────────────────────────────────────── $0.002 ─ ↯rush ─╮
│                                                                           │
│                                                                           │
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
";

        assert_eq!(amp_output_fingerprint(first), amp_output_fingerprint(second));
    }

    #[test]
    fn tracker_keeps_amp_waiting_when_compact_chrome_changes() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%30", "amp", false);
        let now = Instant::now();
        let first_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
 ┃ hello

 Hello!
╭────────────────────────────────────────────────────────── $0.001 ─ ↯rush ─╮
│                                                                           │
│                                                                           │
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
"
            .to_string(),
        )]);
        let second_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
 ┃ hello

 Hello!
╭────────────────────────────────────────────────────────── $0.002 ─ ↯rush ─╮
│                                                                           │
│                                                                           │
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
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

        assert_eq!(second[0].status, SessionStatus::WaitingInput);
    }

    #[test]
    fn tracker_marks_amp_running_when_bottom_status_changes() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%30", "amp", false);
        let now = Instant::now();
        let waiting_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
 ┃ hello

 Hello!
╭────────────────────────────────────────────────────────── $0.001 ─ ↯rush ─╮
│                                                                           │
│                                                                           │
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
"
            .to_string(),
        )]);
        let running_output = HashMap::from([(
            pane.pane_id.clone(),
            "\
 ┃ hello

 Hello!
╭────────────────────────────────────────────────────────── $0.001 ─ ↯rush ─╮
│ streaming tokens                                                          │
│                                                                           │
╰──────────────────────────────────────────────────── ~/workspace (main) ─╯
"
            .to_string(),
        )]);

        let first = tracker.refresh(std::slice::from_ref(&pane), &waiting_output, now);
        assert_eq!(first[0].status, SessionStatus::WaitingInput);

        let second = tracker.refresh(
            std::slice::from_ref(&pane),
            &running_output,
            now + Duration::from_secs(5),
        );

        assert_eq!(second[0].status, SessionStatus::Running);
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
│      Sonnet 4.6 · Claude Pro · example.com's Organization          │
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
~/workspace:main             ctrl+t variants  tab agents  ctrl+p commands
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
~/workspace/specs/demo (main)
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
                "gpt-5.4 xhigh fast · 100% left · ~/workspace".to_string(),
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
    fn tracker_retains_previous_gemini_output_when_new_tail_has_only_footer_hints() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot_with_title("%26", "node", false, "◇  Ready (workspace)");
        let now = Instant::now();
        let first = tracker.refresh(
            std::slice::from_ref(&pane),
            &HashMap::from([(
                pane.pane_id.clone(),
                "\
✦ hello

? for shortcuts
shift+tab to accept edits
>   Type your message or @path/to/file
~                             no sandbox (see /docs)                              /model Auto (Gemini 3)
"
                .to_string(),
            )]),
            now,
        );

        assert_eq!(first[0].output_excerpt.as_deref(), Some("hello"));

        let second = tracker.refresh(
            std::slice::from_ref(&pane),
            &HashMap::from([(
                pane.pane_id.clone(),
                "\
shift+tab to accept edits
>   Type your message or @path/to/file
~                             no sandbox (see /docs)                              /model Auto (Gemini 3)
"
                .to_string(),
            )]),
            now + Duration::from_secs(5),
        );

        assert_eq!(second[0].output_excerpt.as_deref(), Some("hello"));
    }

    #[test]
    fn tracker_reuses_detail_and_output_excerpt_arcs_when_values_do_not_change() {
        let mut tracker = SessionTracker::new();
        let pane = snapshot("%15", "codex", false);
        let now = Instant::now();
        let output_tail =
            "gpt-5.4 xhigh fast · 100% left · ~/workspace\nHello from the current turn.";
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
            "{pane_id}\t301\t$5\tclient\t@8\tagents\t{}\t/workspace/ilmari\t{pane_current_command}\t{pane_title}",
            if pane_dead { 1 } else { 0 }
        ))
        .expect("pane snapshot should parse")
    }

    fn process_title_fixtures() -> Vec<(AgentKind, &'static str)> {
        include_str!("fixtures/process_titles.tsv")
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .map(|line| {
                let fields: Vec<_> = line.split('\t').collect();
                assert_eq!(fields.len(), 2, "invalid process title fixture: {line}");
                (fixture_kind(fields[0]), fields[1])
            })
            .collect()
    }

    fn pane_title_fixtures() -> Vec<(AgentKind, &'static str, &'static str)> {
        include_str!("fixtures/pane_titles.tsv")
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .map(|line| {
                let fields: Vec<_> = line.split('\t').collect();
                assert_eq!(fields.len(), 3, "invalid pane title fixture: {line}");
                (fixture_kind(fields[0]), fields[1], fields[2])
            })
            .collect()
    }

    fn output_snippet_fixtures() -> Vec<(AgentKind, &'static str, &'static str, &'static str)> {
        vec![
            (
                AgentKind::ClaudeCode,
                "node",
                "⠐ Plan and start MIR-1094",
                include_str!("fixtures/output_claude.txt"),
            ),
            (AgentKind::GeminiCli, "node", "worker", include_str!("fixtures/output_gemini.txt")),
            (
                AgentKind::AntigravityCli,
                "node",
                "worker",
                include_str!("fixtures/output_antigravity.txt"),
            ),
            (AgentKind::Grok, "node", "worker", include_str!("fixtures/output_grok.txt")),
        ]
    }

    fn planned_command_fixtures() -> Vec<(AgentKind, &'static str)> {
        vec![
            (AgentKind::GitHubCopilotCli, "copilot"),
            (AgentKind::CursorCli, "cursor"),
            (AgentKind::Aider, "aider"),
            (AgentKind::ClineCli, "cline"),
            (AgentKind::GooseCli, "goose"),
            (AgentKind::KiroCli, "kiro-cli"),
            (AgentKind::OpenHandsCli, "openhands"),
        ]
    }

    fn fixture_kind(value: &str) -> AgentKind {
        match value {
            "Codex" => AgentKind::Codex,
            "Amp" => AgentKind::Amp,
            "ClaudeCode" => AgentKind::ClaudeCode,
            "OpenCode" => AgentKind::OpenCode,
            "Pi" => AgentKind::Pi,
            "GeminiCli" => AgentKind::GeminiCli,
            "AntigravityCli" => AgentKind::AntigravityCli,
            "Auggie" => AgentKind::Auggie,
            "Grok" => AgentKind::Grok,
            other => panic!("unknown fixture agent kind: {other}"),
        }
    }

    // Fixtures based on live Grok panes, trimmed to the parser-relevant lines.
    const GROK_WAITING_NARROW: &str = "\
     ❯ hello 12:19 PM

     ◆ Thought for 3.6s
     ◆ List .
     ◆ Thought for 0.5s
     ◆ Read README.md
     ◆ Thought for 0.6s

     Hello! 👋 12:19 PM

     Welcome to the workspace. Clean main branch, Rust
     project.

     What would you like to work on?

     Turn completed in 22s.

  ╭──────────────────────────────────────────────────────────────────────╮
  │ ❯                                                                    │
  ╰───────────────────────────────────────────────────────── Grok Build ─╯

  Shift+Tab:mode  │  Ctrl+.:shortcuts
";

    const GROK_WAITING_WIDE: &str = "\
     ❯ hello 12:16 PM

     Hi! I'm Grok 4.3. 12:16 PM
     █
     Welcome to the workspace. What can I help you with? █
     Turn completed in 10s. █

  ╭──────────────────────────────────────────────────────────────────────╮
  │ ❯                                                                    │
  ╰───────────────────────────────────────── Grok Build · always-approve ─╯

  Shift+Tab:mode  │  Ctrl+.:shortcuts
";

    const GROK_COMPACT_WAITING: &str = "\
➜  ilmari
  main ~/workspace │ 23K / 512K │
    Please reply with exactly: compact fixture ready 1:40 PM

    ◆ Thought for 0.7s

    compact fixture ready 1:40 PM

    Turn completed in 3.4s.

 ╭──────────────────────────────────────────────────────────────────────╮
 │ ❯                                                                    │
 ╰───────────────────────────────────────── Grok Build · always-approve ─╯
 Shift+Tab:mode  │  Ctrl+.:shortcuts
";

    const GROK_ACTIVE_WAITING_FOOTER: &str = "\
   ⠹ Waiting… 7m1s                                                        45m8s ⇣239k [✗]
 ╭──────────────────────────────────────────────────────────────────────╮
 │ ❯                                                                    │
 ╰───────────────────────────────────────── Grok Build · always-approve ─╯
 Shift+Tab:mode  │  Ctrl+c:cancel  │  Ctrl+Enter:interject  │  Ctrl+.:shortcuts
";

    const GROK_ACTIVE_WAITING_FOOTER_UPDATED: &str = "\
   ⠴ Waiting… 7m6s                                                       45m13s ⇣239k [✗]
 ╭──────────────────────────────────────────────────────────────────────╮
 │ ❯                                                                    │
 ╰───────────────────────────────────────── Grok Build · always-approve ─╯
 Shift+Tab:mode  │  Ctrl+c:cancel  │  Ctrl+Enter:interject  │  Ctrl+.:shortcuts
";

    const GROK_COMPLETION_ONLY: &str = "Turn completed in 5m57s.";

    const GROK_COMPACT_COMMAND_PALETTE: &str = "\
────────────────────────────────────────────────────────────────────────1─
   ❯ /compact-mode Toggle compact UI (less padding, more content)
──────────────────────────────────────────────────────────────────────────
╭────────────────────────────────────────────────────────────────────────╮
│ ❯ /compact-mode                                                        │
╰────────────────────────────────────────── Grok Build · always-approve ─╯
Enter:send  │  Shift+Tab:mode  │  Ctrl+.:shortcuts
";

    const GROK_BUSY_SAMPLE: &str = "\
     ◆ Tmux Capture-pane
     ◆ Thought for 0.5s
     ⏳
     ⠦ - Thinking - ... - grok
     • Ran git ...
     ⚠ Automatic approval review ok
     ◆ Thought for 27.3s
     ⠼ - Thinking...
";

    const GROK_CHOOSER_SAMPLE: &str = "\
Grok Build Beta

New worktree
... ascii art ...
Try out Grok Build
";

    #[test]
    fn classify_grok_output_tail_detects_waiting_from_fixtures() {
        assert_eq!(classify_output_tail(GROK_WAITING_NARROW), None);
        assert_eq!(classify_output_tail(GROK_WAITING_WIDE), None);
        assert_eq!(classify_output_tail(GROK_COMPACT_WAITING), None);
        assert_eq!(classify_output_tail(GROK_COMPACT_COMMAND_PALETTE), None);
        assert_eq!(classify_output_tail(GROK_COMPLETION_ONLY), None);
        assert_eq!(
            classify_grok_output_tail(GROK_WAITING_NARROW),
            Some(SessionStatus::WaitingInput)
        );
        assert_eq!(classify_grok_output_tail(GROK_WAITING_WIDE), Some(SessionStatus::WaitingInput));
        assert_eq!(
            classify_grok_output_tail(GROK_COMPACT_WAITING),
            Some(SessionStatus::WaitingInput)
        );
        assert_eq!(
            classify_grok_output_tail(GROK_COMPACT_COMMAND_PALETTE),
            Some(SessionStatus::WaitingInput)
        );
        assert_eq!(
            classify_grok_output_tail(GROK_COMPLETION_ONLY),
            Some(SessionStatus::WaitingInput)
        );
        // busy sample (no turn/prompt trigger) does not classify as waiting from tail alone
        assert_eq!(classify_grok_output_tail(GROK_BUSY_SAMPLE), None);
        // chooser screen also triggers waiting via "grok build" in looks_like (sensible for launch chooser)
        assert_eq!(classify_output_tail(GROK_CHOOSER_SAMPLE), None);
        assert_eq!(
            classify_grok_output_tail(GROK_CHOOSER_SAMPLE),
            Some(SessionStatus::WaitingInput)
        );

        // detail extraction (WIDE has version; NARROW yields None per "Grok Build" not version)
        assert_eq!(
            extract_grok_detail(Some(GROK_WAITING_WIDE)),
            Some(AgentDetail { label: "Grok 4.3".to_string(), tone: AgentDetailTone::Neutral })
        );
        assert_eq!(
            extract_grok_detail(Some(
                "╰────────────────────────────────────── Composer 2.5 · always-approve ─╯"
            )),
            Some(AgentDetail { label: "Composer 2.5".to_string(), tone: AgentDetailTone::Neutral })
        );
        assert_eq!(extract_grok_detail(Some(GROK_WAITING_NARROW)), None);
        assert_eq!(extract_grok_detail(Some("I asked Grok 4.3. about the project.")), None);
        assert_eq!(extract_grok_detail(Some("I asked Composer 2.5 about the project.")), None);
    }

    #[test]
    fn grok_prompt_and_completion_do_not_override_newer_output() {
        let output_tail = "\
     Turn completed in 5m57s.

  ╭──────────────────────────────────────────────────────────────────────╮
  │ ❯                                                                    │
  ╰───────────────────────────────────────── Grok Build · always-approve ─╯

  Shift+Tab:mode  │  Ctrl+.:shortcuts
     Streaming another tool call after the old prompt.
";

        assert_eq!(classify_output_tail(output_tail), None);
        assert_eq!(classify_grok_output_tail(output_tail), None);
        assert_eq!(
            extract_grok_output_excerpt(Some(output_tail)),
            Some("Streaming another tool call after the old prompt.".to_string())
        );
    }

    #[test]
    fn grok_output_excerpt_prefers_latest_reply_before_completion_status() {
        assert_eq!(
            extract_grok_output_excerpt(Some(GROK_WAITING_NARROW)),
            Some("What would you like to work on?".to_string())
        );
        assert_eq!(
            extract_grok_output_excerpt(Some(GROK_WAITING_WIDE)),
            Some("Welcome to the workspace. What can I help you with?".to_string())
        );
        assert_eq!(
            extract_grok_output_excerpt(Some(GROK_COMPACT_WAITING)),
            Some("compact fixture ready 1:40 PM".to_string())
        );
        assert_eq!(
            extract_grok_output_excerpt(Some(GROK_COMPLETION_ONLY)),
            Some("Turn completed in 5m57s.".to_string())
        );
        assert_eq!(extract_grok_output_excerpt(Some(GROK_COMPACT_COMMAND_PALETTE)), None);

        let current_pane_tail = "\
     Final answer text.

     Turn completed in 5m57s.

  ╭──────────────────────────────────────────────────────────────────────╮
  │ ❯                                                                    │
  ╰───────────────────────────────────────── Grok Build · always-approve ─╯

  Shift+Tab:mode  │  Ctrl+.:shortcuts
";

        assert_eq!(
            extract_grok_output_excerpt(Some(current_pane_tail)),
            Some("Final answer text.".to_string())
        );

        let composer_tail = "\
     ❯ hi                                                         7:58 PM

     ◆ Thought for 0.0s

     Hi! How can I help you today? I see you're working in the    7:58 PM
     workspace project - happy to dig into code, debug, or
     implement something whenever you're ready.

     Turn completed in 2.2s.

  ╭───────────────────────────────────────────────────────────────────────╮
  │ ❯                                                                     │
  ╰─────────────────────────────────────── Composer 2.5 · always-approve ─╯

  Shift+Tab:mode  │  Ctrl+.:shortcuts
";

        assert_eq!(
            extract_grok_output_excerpt(Some(composer_tail)),
            Some(
                "... happy to dig into code, debug, or implement something whenever you're ready."
                    .to_string()
            )
        );
    }

    #[test]
    fn grok_adapter_detects_from_command_and_title() {
        let registry = AdapterRegistry::v1();
        let grok_cmd = snapshot("%19", "grok-macos-aarc", false);
        let grok_title = snapshot_with_title("%19", "node", false, "grok-easy-2nd-pane");
        let stale_shell_title = snapshot_with_title("%19", "zsh", false, "grok-easy-2nd-pane");
        let grok_title_build = snapshot_with_title(
            "%20",
            "grok-macos-aarc",
            false,
            "Starting New User Session with Hello Mes… - grok",
        );

        assert_eq!(registry.detect_kind(&grok_cmd, None), Some(AgentKind::Grok));
        assert_eq!(registry.detect_kind(&grok_title, None), Some(AgentKind::Grok));
        assert_eq!(registry.detect_kind(&grok_title_build, None), Some(AgentKind::Grok));
        assert_eq!(registry.detect_kind(&stale_shell_title, None), None);
    }

    #[test]
    fn grok_tail_classifier_stays_scoped_to_grok_adapter() {
        let pane = snapshot("%21", "node", false);

        assert_eq!(
            super::CodexAdapter.classify(&pane, Some(GROK_WAITING_NARROW), None, None),
            SessionStatus::Unknown
        );
        assert_eq!(
            super::GrokAdapter.classify(&pane, Some(GROK_WAITING_NARROW), None, None),
            SessionStatus::WaitingInput
        );
    }

    #[test]
    fn tracker_full_grok_from_listpanes_format_and_output_tail() {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        // exact tab-separated format from tmux list-panes -F as used by ilmari and captured live
        let pane19 = PaneSnapshot::parse(
            "%19\t27874\t$4\t4\t@8\tzsh\t0\t/workspace/ilmari\tgrok-macos-aarc\tgrok-easy-2nd-pane",
        )
        .expect("parse pane19");
        let pane20 = PaneSnapshot::parse(
            "%20\t54502\t$4\t4\t@11\teasymode-test\t0\t/workspace/ilmari\tgrok-macos-aarc\tStarting New User Session with Hello Mes… - grok"
        ).expect("parse pane20");
        let output_tails = HashMap::from([
            (pane19.pane_id.clone(), GROK_WAITING_NARROW.to_string()),
            (pane20.pane_id.clone(), GROK_WAITING_WIDE.to_string()),
        ]);

        let records = tracker.refresh(&[pane19.clone(), pane20.clone()], &output_tails, now);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].kind.display_name(), "Grok");
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
        assert_eq!(records[0].output_excerpt.as_deref(), Some("What would you like to work on?"));

        assert_eq!(records[1].kind.display_name(), "Grok");
        assert_eq!(records[1].status, SessionStatus::WaitingInput);
        assert_eq!(
            records[1].output_excerpt.as_deref(),
            Some("Welcome to the workspace. What can I help you with?")
        );

        // grok detail retention across refreshes with stable tail (exercises reuse_detail_arc for GrokAdapter)
        let now2 = now + Duration::from_secs(1);
        let records2 = tracker.refresh(&[pane19, pane20], &output_tails, now2);
        let wide2 = records2
            .iter()
            .find(|r| r.kind == AgentKind::Grok && r.pane.pane_id == "%20")
            .expect("found grok wide on second refresh");
        assert!(wide2.detail.is_some());
        assert_eq!(wide2.detail.as_ref().unwrap().label, "Grok 4.3");
    }

    #[test]
    fn tracker_full_grok_from_compact_output_tail() {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let pane = PaneSnapshot::parse(
            "%31\t27874\t$6\t6\t@17\tzsh\t0\t/workspace/ilmari\tgrok-macos-aarc\tCompact Grok",
        )
        .expect("parse compact grok pane");
        let output_tails =
            HashMap::from([(pane.pane_id.clone(), GROK_COMPACT_WAITING.to_string())]);

        let records = tracker.refresh(&[pane], &output_tails, now);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::Grok);
        assert_eq!(records[0].status, SessionStatus::WaitingInput);
        assert_eq!(records[0].output_excerpt.as_deref(), Some("compact fixture ready 1:40 PM"));
    }

    #[test]
    fn tracker_marks_grok_active_waiting_footer_as_running() {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let pane = snapshot("%34", "grok-macos-aarc", false);
        let output_tails =
            HashMap::from([(pane.pane_id.clone(), GROK_ACTIVE_WAITING_FOOTER.to_string())]);

        let records = tracker.refresh(std::slice::from_ref(&pane), &output_tails, now);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::Grok);
        assert_eq!(records[0].status, SessionStatus::Running);
        assert_eq!(records[0].output_excerpt.as_deref(), Some("⠹ Waiting… 7m1s 45m8s ⇣239k [✗]"));
    }

    #[test]
    fn tracker_keeps_grok_active_waiting_footer_running_when_timer_changes() {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let pane = snapshot("%35", "grok-macos-aarc", false);

        let first = tracker.refresh(
            std::slice::from_ref(&pane),
            &HashMap::from([(pane.pane_id.clone(), GROK_ACTIVE_WAITING_FOOTER.to_string())]),
            now,
        );
        let second = tracker.refresh(
            std::slice::from_ref(&pane),
            &HashMap::from([(
                pane.pane_id.clone(),
                GROK_ACTIVE_WAITING_FOOTER_UPDATED.to_string(),
            )]),
            now + Duration::from_secs(5),
        );

        assert_eq!(first[0].status, SessionStatus::Running);
        assert_eq!(second[0].status, SessionStatus::Running);
        assert_ne!(second[0].output_fingerprint, first[0].output_fingerprint);
        assert_eq!(second[0].last_changed_at, first[0].last_changed_at);
    }

    #[test]
    fn tracker_marks_grok_finished_when_shell_return_keeps_grok_title() {
        let mut tracker = SessionTracker::with_retention(Duration::from_secs(30));
        let now = Instant::now();
        let running_pane = snapshot_with_title("%32", "grok-macos-aarc", false, "Compact Grok");

        tracker.refresh(&[running_pane], &HashMap::new(), now);

        let shell_pane = snapshot_with_title("%32", "zsh", false, "Compact Grok");
        let records = tracker.refresh(
            std::slice::from_ref(&shell_pane),
            &HashMap::new(),
            now + Duration::from_secs(5),
        );

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, AgentKind::Grok);
        assert_eq!(records[0].status, SessionStatus::Finished);
        assert_eq!(records[0].pane.pane_id, shell_pane.pane_id);
        assert!(records[0].retained_until.is_some());
    }

    #[test]
    fn tracker_marks_grok_waiting_when_completion_arrives_after_busy_output() {
        let mut tracker = SessionTracker::new();
        let now = Instant::now();
        let pane = snapshot("%33", "grok-macos-aarc", false);
        let waiting = HashMap::from([(pane.pane_id.clone(), GROK_WAITING_NARROW.to_string())]);
        let busy = HashMap::from([(pane.pane_id.clone(), GROK_BUSY_SAMPLE.to_string())]);
        let completed = HashMap::from([(pane.pane_id.clone(), GROK_COMPLETION_ONLY.to_string())]);

        let first = tracker.refresh(std::slice::from_ref(&pane), &waiting, now);
        assert_eq!(first[0].status, SessionStatus::WaitingInput);

        let second =
            tracker.refresh(std::slice::from_ref(&pane), &busy, now + Duration::from_secs(5));
        assert_eq!(second[0].status, SessionStatus::Running);
        assert_ne!(second[0].output_fingerprint, first[0].output_fingerprint);

        let third =
            tracker.refresh(std::slice::from_ref(&pane), &completed, now + Duration::from_secs(10));
        assert_eq!(third[0].status, SessionStatus::WaitingInput);
        assert_eq!(third[0].output_excerpt.as_deref(), Some("Turn completed in 5m57s."));
        assert_ne!(third[0].output_fingerprint, second[0].output_fingerprint);
    }
}
