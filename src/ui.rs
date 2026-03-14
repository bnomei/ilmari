use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};
use ratatui::Frame;

use crate::colors::{Palette, SemanticRole};
use crate::model::{
    AgentDetailTone, AppModel, GitSummaryRow, PaneRow, ResourceUsage, SessionStatus, WorkspaceGroup,
};

const FOOTER_BRAND: &str = "🅸 🅻 🅼 🅰 🆁 🅸 ";
const FOOTER_BRAND_WIDTH: u16 = 18;
const FOOTER_COMPACT_BREAKPOINT: u16 = 80;
const TIME_COL_WIDTH: usize = 5;
const OUTPUT_COL_WIDTH: usize = 80;
const PANE_INLINE_WIDTH: usize = 4;
const AGENT_COL_WIDTH: usize = 12;
const DETAIL_COL_WIDTH: usize = 24;
const SELF_COL_WIDTH: usize = 14;
const SUB_COL_WIDTH: usize = 14;
const STATUS_COL_WIDTH: usize = 1;

pub fn render(frame: &mut Frame, model: &AppModel, palette: &Palette) {
    let sections =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1), Constraint::Length(1)])
            .split(frame.area());
    frame.render_widget(AppView::new(model, palette), sections[0]);
    frame.render_widget(FooterView::new(palette), sections[2]);
}

struct AppView<'a> {
    model: &'a AppModel,
    palette: &'a Palette,
}

impl<'a> AppView<'a> {
    fn new(model: &'a AppModel, palette: &'a Palette) -> Self {
        Self { model, palette }
    }
}

impl Widget for AppView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        Block::default().style(self.palette.base_style()).render(area, buf);

        let status_height = u16::from(!self.model.status_line.is_empty());
        let sections =
            Layout::vertical([Constraint::Length(status_height), Constraint::Min(0)]).split(area);

        if status_height > 0 {
            Paragraph::new(self.model.status_line.as_str())
                .style(self.palette.base_style())
                .render(sections[0], buf);
        }
        Paragraph::new(workspace_lines(self.model, self.palette, sections[1].width as usize))
            .style(self.palette.base_style())
            .render(sections[1], buf);
    }
}

struct FooterView<'a> {
    palette: &'a Palette,
}

impl<'a> FooterView<'a> {
    fn new(palette: &'a Palette) -> Self {
        Self { palette }
    }
}

impl Widget for FooterView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        render_footer(area, buf, self.palette);
    }
}

fn workspace_lines(
    model: &AppModel,
    palette: &Palette,
    available_width: usize,
) -> Vec<Line<'static>> {
    if model.workspace_groups.is_empty() {
        return vec![
            Line::from(Span::styled("none", palette.style_for(SemanticRole::HeadingAccent))),
            Line::from(Span::styled(
                "  no supported agent sessions detected",
                palette.style_for(SemanticRole::MutedText),
            )),
        ];
    }

    let mut lines = Vec::new();
    for (index, group) in model.workspace_groups.iter().enumerate() {
        if index > 0 {
            lines.push(Line::from(""));
        }

        lines.extend(workspace_header_lines(group, palette, model.show_git, available_width));
        for row in &group.rows {
            lines.push(workspace_row_line(
                row,
                palette,
                model.show_app,
                model.show_detail,
                model.show_time,
                model.show_output,
                model.show_stats,
            ));
            if row.subtasks_expanded {
                lines.extend(subtask_lines(
                    row,
                    palette,
                    model.show_app,
                    model.show_detail,
                    model.show_time,
                    model.show_output,
                    model.show_stats,
                ));
            }
        }
    }

    lines
}

fn workspace_header_lines(
    group: &WorkspaceGroup,
    palette: &Palette,
    show_git: bool,
    available_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![workspace_header_line(group, palette, available_width)];
    if show_git && group.git_summary.is_some() {
        lines.extend(workspace_git_summary_lines(group, palette, available_width));
    }
    lines
}

fn workspace_header_line(
    group: &WorkspaceGroup,
    palette: &Palette,
    available_width: usize,
) -> Line<'static> {
    workspace_label_line(group, palette, available_width)
}

fn workspace_label_line(
    group: &WorkspaceGroup,
    palette: &Palette,
    available_width: usize,
) -> Line<'static> {
    let label_style =
        palette.style_for(SemanticRole::HeadingAccent).add_modifier(Modifier::REVERSED);
    let label = fit_cell(&group.label, available_width.max(group.label.chars().count()));

    Line::from(vec![Span::styled(label, label_style)])
}

fn workspace_git_summary_lines(
    group: &WorkspaceGroup,
    palette: &Palette,
    available_width: usize,
) -> Vec<Line<'static>> {
    let Some(summary) = &group.git_summary else {
        return Vec::new();
    };

    let stats_text = format_git_stats_text(summary);
    let combined_width = summary.branch_name.chars().count() + 2 + stats_text.chars().count();

    if combined_width > available_width {
        return vec![
            Line::from(vec![Span::styled(
                summary.branch_name.clone(),
                palette.style_for(SemanticRole::MutedText),
            )]),
            workspace_git_stats_line(summary, palette),
        ];
    }

    vec![Line::from(vec![
        Span::styled(summary.branch_name.clone(), palette.style_for(SemanticRole::MutedText)),
        Span::raw("  "),
        Span::styled(
            format!("+{}", summary.insertions),
            palette.style_for(SemanticRole::GitInsertions),
        ),
        Span::raw(" "),
        Span::styled(
            format!("-{}", summary.deletions),
            palette.style_for(SemanticRole::GitDeletions),
        ),
    ])]
}

fn workspace_git_stats_line(summary: &GitSummaryRow, palette: &Palette) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("+{}", summary.insertions),
            palette.style_for(SemanticRole::GitInsertions),
        ),
        Span::raw(" "),
        Span::styled(
            format!("-{}", summary.deletions),
            palette.style_for(SemanticRole::GitDeletions),
        ),
    ])
}

fn format_git_stats_text(summary: &GitSummaryRow) -> String {
    format!("+{} -{}", summary.insertions, summary.deletions)
}

fn push_cell(
    spans: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    content: String,
    style: Style,
) {
    if !spans.is_empty() {
        push_raw_spaces(spans, current_width, 1);
    }
    *current_width += content.chars().count();
    spans.push(Span::styled(content, style));
}

fn push_inline_span(
    spans: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    content: String,
    style: Style,
) {
    *current_width += content.chars().count();
    spans.push(Span::styled(content, style));
}

fn push_raw_spaces(spans: &mut Vec<Span<'static>>, current_width: &mut usize, count: usize) {
    if count == 0 {
        return;
    }

    *current_width += count;
    spans.push(Span::raw(" ".repeat(count)));
}

fn workspace_row_line(
    row: &PaneRow,
    palette: &Palette,
    show_app: bool,
    show_detail: bool,
    show_time: bool,
    show_output: bool,
    show_stats: bool,
) -> Line<'static> {
    let status_style = palette.style_for(status_role(row.status));
    let pane_label_style = pane_label_style(row, palette);
    let mut spans = Vec::new();
    let mut current_width = 0;
    if show_time {
        let time_label = if row.inactive_since_label.is_empty() {
            " ".repeat(TIME_COL_WIDTH)
        } else {
            truncate_cell(&row.inactive_since_label, TIME_COL_WIDTH)
        };
        push_inline_span(
            &mut spans,
            &mut current_width,
            time_label,
            palette.style_for(SemanticRole::MutedText),
        );
        push_raw_spaces(&mut spans, &mut current_width, 1);
    }
    push_inline_span(
        &mut spans,
        &mut current_width,
        truncate_cell(status_symbol(row.status), STATUS_COL_WIDTH),
        status_style,
    );
    push_raw_spaces(&mut spans, &mut current_width, 1);
    push_inline_span(&mut spans, &mut current_width, row.pane_id.clone(), pane_label_style);
    push_raw_spaces(
        &mut spans,
        &mut current_width,
        PANE_INLINE_WIDTH.saturating_sub(row.pane_id.chars().count()),
    );

    if show_app {
        push_cell(
            &mut spans,
            &mut current_width,
            truncate_cell(row.client_label, AGENT_COL_WIDTH),
            palette.style_for(SemanticRole::AppLabel).add_modifier(Modifier::BOLD),
        );
    }

    if show_detail {
        let (label, style) = match &row.detail {
            Some(detail) => (
                truncate_cell(&detail.label, DETAIL_COL_WIDTH),
                palette.style_for(detail_role(detail.tone)),
            ),
            None => (String::new(), palette.style_for(SemanticRole::MutedText)),
        };
        push_cell(&mut spans, &mut current_width, label, style);
    }

    if show_stats {
        if let Some(process_usage) = row.process_usage.as_ref() {
            push_cell(
                &mut spans,
                &mut current_width,
                truncate_cell(&format_usage_chip(process_usage.agent), SELF_COL_WIDTH),
                palette.style_for(SemanticRole::HeadingAccent).add_modifier(Modifier::BOLD),
            );
            push_cell(
                &mut spans,
                &mut current_width,
                truncate_cell(&format_usage_chip(process_usage.spawned), SUB_COL_WIDTH),
                palette.style_for(SemanticRole::AgentDetailNeutral),
            );
        } else {
            push_cell(
                &mut spans,
                &mut current_width,
                String::new(),
                palette.style_for(SemanticRole::MutedText),
            );
            push_cell(
                &mut spans,
                &mut current_width,
                String::new(),
                palette.style_for(SemanticRole::MutedText),
            );
        }
    }

    if show_output {
        push_cell(
            &mut spans,
            &mut current_width,
            truncate_cell(row.output_excerpt.as_deref().unwrap_or_default(), OUTPUT_COL_WIDTH),
            palette.base_style(),
        );
    }

    if row.is_selected {
        let selected_style = selected_row_style(palette);
        for span in &mut spans {
            span.style = selected_style;
        }
        return Line::from(spans).style(selected_style);
    }

    Line::from(spans)
}

fn pane_label_style(row: &PaneRow, palette: &Palette) -> Style {
    let style = palette.style_for(SemanticRole::HeadingAccent);
    if row.is_jump_match {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn subtask_lines(
    row: &PaneRow,
    palette: &Palette,
    show_app: bool,
    _show_detail: bool,
    show_time: bool,
    show_output: bool,
    show_stats: bool,
) -> Vec<Line<'static>> {
    let Some(process_usage) = row.process_usage.as_ref() else {
        return Vec::new();
    };

    process_usage
        .subtasks
        .iter()
        .map(|subtask| {
            let detail_cell = format!(
                "{}- {} #{}",
                "  ".repeat(subtask.depth + 1),
                subtask.command_label,
                subtask.pid
            );
            let mut spans = Vec::new();
            let mut current_width = 0;
            push_raw_spaces(&mut spans, &mut current_width, prefix_cluster_width(show_time));
            if show_app {
                push_cell(
                    &mut spans,
                    &mut current_width,
                    String::new(),
                    palette.style_for(SemanticRole::MutedText),
                );
            }
            push_cell(
                &mut spans,
                &mut current_width,
                truncate_cell(&detail_cell, DETAIL_COL_WIDTH),
                palette.style_for(SemanticRole::MutedText),
            );
            if show_stats {
                push_cell(
                    &mut spans,
                    &mut current_width,
                    String::new(),
                    palette.style_for(SemanticRole::MutedText),
                );
                push_cell(
                    &mut spans,
                    &mut current_width,
                    truncate_cell(&format_usage_chip(subtask.usage), SUB_COL_WIDTH),
                    palette.style_for(SemanticRole::MutedText),
                );
            }
            if show_output {
                push_cell(
                    &mut spans,
                    &mut current_width,
                    String::new(),
                    palette.style_for(SemanticRole::MutedText),
                );
            }
            Line::from(spans)
        })
        .collect()
}

fn prefix_cluster_width(show_time: bool) -> usize {
    let time_width = if show_time { TIME_COL_WIDTH + 1 } else { 0 };
    time_width + STATUS_COL_WIDTH + 1 + PANE_INLINE_WIDTH
}

fn status_symbol(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Running => "▶",
        SessionStatus::WaitingInput => "●",
        SessionStatus::Finished => "✔",
        SessionStatus::Terminated => "✖",
        SessionStatus::Unknown => "?",
    }
}

fn selected_row_style(palette: &Palette) -> Style {
    palette.base_style().add_modifier(Modifier::BOLD | Modifier::REVERSED)
}

fn render_footer(area: Rect, buf: &mut Buffer, palette: &Palette) {
    Block::default().style(palette.base_style()).render(area, buf);

    let key_style = palette.style_for(SemanticRole::HeadingAccent).add_modifier(Modifier::BOLD);
    let sep_style = palette.style_for(SemanticRole::MutedText);
    let footer = if area.width < FOOTER_COMPACT_BREAKPOINT {
        compact_footer_line(key_style, sep_style)
    } else {
        footer_line(key_style, sep_style)
    };
    let left = Paragraph::new(footer).alignment(Alignment::Left).style(palette.base_style());

    if area.width > FOOTER_BRAND_WIDTH {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(FOOTER_BRAND_WIDTH)])
            .split(area);
        left.render(chunks[0], buf);
        Paragraph::new(FOOTER_BRAND)
            .alignment(Alignment::Right)
            .style(key_style)
            .render(chunks[1], buf);
    } else {
        left.render(area, buf);
    }
}

fn footer_line(key_style: Style, sep_style: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let push_sep = |spans: &mut Vec<Span<'static>>| {
        if !spans.is_empty() {
            spans.push(Span::styled(" | ", sep_style));
        }
    };
    let push_item = |spans: &mut Vec<Span<'static>>, label: &'static str, key: &'static str| {
        push_sep(spans);
        spans.push(Span::styled(label, sep_style));
        spans.push(Span::raw(":"));
        spans.push(Span::styled(key, key_style));
    };

    push_item(&mut spans, "Move", "j/k");
    push_item(&mut spans, "Pane", "%");
    push_item(&mut spans, "App", "a");
    push_item(&mut spans, "Bell", "b");
    push_item(&mut spans, "Git", "g");
    push_item(&mut spans, "Model", "m");
    push_item(&mut spans, "Time", "t");
    push_item(&mut spans, "Output", "o");
    push_item(&mut spans, "Stats", "s");
    push_item(&mut spans, "Subs", "=");
    push_item(&mut spans, "Jump", "Enter");
    push_item(&mut spans, "Quit", "q");

    Line::from(spans)
}

fn compact_footer_line(key_style: Style, sep_style: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let push_sep = |spans: &mut Vec<Span<'static>>| {
        if !spans.is_empty() {
            spans.push(Span::styled(" ", sep_style));
        }
    };
    let push_key = |spans: &mut Vec<Span<'static>>, key: &'static str| {
        push_sep(spans);
        spans.push(Span::styled(key, key_style));
    };

    for key in ["j", "k", "%", "a", "b", "g", "m", "t", "o", "s", "=", "q"] {
        push_key(&mut spans, key);
    }

    Line::from(spans)
}

fn status_role(status: SessionStatus) -> SemanticRole {
    match status {
        SessionStatus::Running => SemanticRole::StatusRunning,
        SessionStatus::WaitingInput => SemanticRole::StatusWaitingInput,
        SessionStatus::Finished => SemanticRole::StatusFinished,
        SessionStatus::Terminated => SemanticRole::StatusTerminated,
        SessionStatus::Unknown => SemanticRole::StatusUnknown,
    }
}

fn detail_role(tone: AgentDetailTone) -> SemanticRole {
    match tone {
        AgentDetailTone::Neutral => SemanticRole::AgentDetailNeutral,
        AgentDetailTone::Positive => SemanticRole::AgentDetailPositive,
        AgentDetailTone::Warning => SemanticRole::AgentDetailWarning,
    }
}

fn format_cpu(usage: ResourceUsage) -> String {
    format!("{}.{}%", usage.cpu_tenths_percent / 10, usage.cpu_tenths_percent % 10)
}

fn format_memory(usage: ResourceUsage) -> String {
    format_memory_kib(usage.memory_kib)
}

fn format_resource_usage(usage: ResourceUsage) -> String {
    format!("{}/{}", format_cpu(usage), format_memory(usage))
}

fn format_usage_chip(usage: ResourceUsage) -> String {
    format!("[{}]", format_resource_usage(usage))
}

fn fit_cell(value: &str, width: usize) -> String {
    let truncated = truncate_cell(value, width);
    let pad = width.saturating_sub(truncated.chars().count());
    format!("{truncated}{}", " ".repeat(pad))
}

fn truncate_cell(value: &str, width: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= width {
        return value.to_string();
    }

    if width <= 3 {
        return ".".repeat(width);
    }

    let mut truncated = value.chars().take(width - 3).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn format_memory_kib(memory_kib: u64) -> String {
    const KIB_PER_MIB: u64 = 1024;
    const KIB_PER_GIB: u64 = 1024 * 1024;

    if memory_kib < KIB_PER_MIB {
        return format!("{memory_kib}K");
    }

    if memory_kib < KIB_PER_GIB {
        return format!("{}M", (memory_kib + (KIB_PER_MIB / 2)) / KIB_PER_MIB);
    }

    let gib_tenths = (memory_kib.saturating_mul(10) + (KIB_PER_GIB / 2)) / KIB_PER_GIB;
    format!("{}.{}G", gib_tenths / 10, gib_tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::{
        compact_footer_line, footer_line, format_usage_chip, selected_row_style,
        workspace_header_line, workspace_header_lines, workspace_row_line, FOOTER_BRAND,
        FOOTER_COMPACT_BREAKPOINT,
    };
    use crate::colors::{Palette, SemanticRole};
    use crate::model::{
        AgentDetail, AgentDetailTone, AppModel, GitSummaryRow, PaneRow, ResourceUsage,
        SessionProcessUsage, SessionStatus, SubtaskProcess, WorkspaceGroup,
    };
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Modifier};
    use ratatui::Terminal;
    use std::path::PathBuf;
    use std::time::{Duration, Instant, SystemTime};

    #[test]
    fn workspace_row_uses_semantic_status_color_when_not_selected() {
        let row = PaneRow {
            pane_id: "%7".to_string(),
            inactive_since_label: "14:27".to_string(),
            output_excerpt: Some(
                "Could you clarify what you'd like me to help with?".to_string().into(),
            ),
            client_label: "Codex",
            detail: Some(
                AgentDetail {
                    label: "gpt-5.4 xhigh fast".to_string(),
                    tone: AgentDetailTone::Neutral,
                }
                .into(),
            ),
            process_usage: Some(
                SessionProcessUsage {
                    agent: ResourceUsage { cpu_tenths_percent: 154, memory_kib: 64 * 1024 },
                    spawned: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 12 * 1024 },
                    subtasks: vec![SubtaskProcess {
                        pid: 102,
                        depth: 0,
                        command_label: "tmux-mcp-rs".to_string(),
                        usage: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 12 * 1024 },
                    }],
                }
                .into(),
            ),
            subtasks_expanded: false,
            status: SessionStatus::WaitingInput,
            status_label: "waiting-input",
            is_jump_match: false,
            is_selected: false,
        };
        let palette = Palette::default();
        let line = workspace_row_line(&row, &palette, true, true, true, true, true);
        let time_cell = "14:27";
        let output_cell = "Could you clarify what you'd like me to help with?";
        let self_usage_label =
            format_usage_chip(row.process_usage.as_ref().expect("usage should exist").agent);
        let spawned_usage_label =
            format_usage_chip(row.process_usage.as_ref().expect("usage should exist").spawned);
        let detail_label = "gpt-5.4 xhigh fast";
        let self_usage_cell = self_usage_label.as_str();
        let spawned_usage_cell = spawned_usage_label.as_str();
        let status_cell = "●";
        let app_cell = "Codex";

        let detail_span = line
            .spans
            .iter()
            .find(|span| span.content.as_ref() == detail_label)
            .expect("detail span");
        let output_span = line
            .spans
            .iter()
            .find(|span| span.content.as_ref() == output_cell)
            .expect("output span");
        let time_span =
            line.spans.iter().find(|span| span.content.as_ref() == time_cell).expect("time span");
        let self_usage_span = line
            .spans
            .iter()
            .find(|span| span.content.as_ref() == self_usage_cell)
            .expect("self usage span");
        let spawned_usage_span = line
            .spans
            .iter()
            .find(|span| span.content.as_ref() == spawned_usage_cell)
            .expect("spawned usage span");
        let status_span = line
            .spans
            .iter()
            .find(|span| span.content.as_ref() == status_cell)
            .expect("status span");
        let app_span =
            line.spans.iter().find(|span| span.content.as_ref() == app_cell).expect("app span");
        let pane_span =
            line.spans.iter().find(|span| span.content.as_ref() == "%7").expect("pane span");

        assert_eq!(detail_span.style.fg, Some(Color::Indexed(8)));
        assert_eq!(output_span.style.fg, palette.base_style().fg);
        assert_eq!(time_span.style.fg, Some(Color::Indexed(8)));
        assert_eq!(self_usage_span.style.fg, Some(Color::Indexed(12)));
        assert!(self_usage_span.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spawned_usage_span.style.fg, Some(Color::Indexed(8)));
        assert_eq!(status_span.style.fg, Some(Color::Indexed(3)));
        assert_eq!(app_span.style.fg, Some(Color::Indexed(14)));
        assert!(app_span.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(pane_span.style.fg, Some(Color::Indexed(12)));
        assert!(!pane_span.style.add_modifier.contains(Modifier::BOLD));
        assert!(!line.style.add_modifier.contains(Modifier::BOLD));
        assert!(!line.style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn selected_workspace_row_uses_uniform_selection_style_for_all_spans() {
        let row = PaneRow {
            pane_id: "%7".to_string(),
            inactive_since_label: "14:27".to_string(),
            output_excerpt: Some(
                "Could you clarify what you'd like me to help with?".to_string().into(),
            ),
            client_label: "Codex",
            detail: Some(
                AgentDetail {
                    label: "gpt-5.4 xhigh fast".to_string(),
                    tone: AgentDetailTone::Neutral,
                }
                .into(),
            ),
            process_usage: Some(
                SessionProcessUsage {
                    agent: ResourceUsage { cpu_tenths_percent: 154, memory_kib: 64 * 1024 },
                    spawned: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 12 * 1024 },
                    subtasks: vec![SubtaskProcess {
                        pid: 102,
                        depth: 0,
                        command_label: "tmux-mcp-rs".to_string(),
                        usage: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 12 * 1024 },
                    }],
                }
                .into(),
            ),
            subtasks_expanded: false,
            status: SessionStatus::WaitingInput,
            status_label: "waiting-input",
            is_jump_match: true,
            is_selected: true,
        };
        let palette = Palette::default();
        let line = workspace_row_line(&row, &palette, true, true, true, true, true);
        let selected_style = selected_row_style(&palette);

        assert_eq!(line.style.fg, selected_style.fg);
        assert_eq!(line.style.bg, selected_style.bg);
        assert!(line.style.add_modifier.contains(Modifier::BOLD));
        assert!(line.style.add_modifier.contains(Modifier::REVERSED));
        assert!(line.spans.iter().all(|span| span.style.fg == selected_style.fg));
        assert!(line.spans.iter().all(|span| span.style.bg == selected_style.bg));
        assert!(line.spans.iter().all(|span| span.style.add_modifier.contains(Modifier::BOLD)));
        assert!(line.spans.iter().all(|span| span.style.add_modifier.contains(Modifier::REVERSED)));
    }

    #[test]
    fn workspace_row_places_time_before_status_and_pane_when_visible() {
        let row = PaneRow {
            pane_id: "%7".to_string(),
            inactive_since_label: "14:27".to_string(),
            output_excerpt: None,
            client_label: "Codex",
            detail: None,
            process_usage: None,
            subtasks_expanded: false,
            status: SessionStatus::WaitingInput,
            status_label: "waiting-input",
            is_jump_match: false,
            is_selected: false,
        };
        let palette = Palette::default();
        let line = workspace_row_line(&row, &palette, false, false, true, false, false);
        let time_cell = "14:27";
        let status_cell = "●";
        let pane_cell = "%7";
        let time_position = line
            .spans
            .iter()
            .position(|span| span.content.as_ref() == time_cell)
            .expect("time span");
        let status_position = line
            .spans
            .iter()
            .position(|span| span.content.as_ref() == status_cell)
            .expect("status span");
        let pane_position = line
            .spans
            .iter()
            .position(|span| span.content.as_ref() == pane_cell)
            .expect("pane span");

        assert!(time_position < status_position);
        assert!(status_position < pane_position);
    }

    #[test]
    fn workspace_row_keeps_blank_time_slot_for_running_rows() {
        let row = PaneRow {
            pane_id: "%7".to_string(),
            inactive_since_label: String::new(),
            output_excerpt: None,
            client_label: "Codex",
            detail: None,
            process_usage: None,
            subtasks_expanded: false,
            status: SessionStatus::Running,
            status_label: "running",
            is_jump_match: false,
            is_selected: false,
        };
        let palette = Palette::default();
        let line = workspace_row_line(&row, &palette, false, false, true, false, false);

        assert_eq!(line.spans[0].content.as_ref(), "     ");
        assert_eq!(line.spans[1].content.as_ref(), " ");
        assert_eq!(line.spans[2].content.as_ref(), "▶");
    }

    #[test]
    fn workspace_row_uses_detail_tone_semantics_for_amp_mode() {
        let row = PaneRow {
            pane_id: "%8".to_string(),
            inactive_since_label: String::new(),
            output_excerpt: Some(
                "Could you clarify what you'd like me to help with?".to_string().into(),
            ),
            client_label: "Amp",
            detail: Some(
                AgentDetail { label: "smart".to_string(), tone: AgentDetailTone::Positive }.into(),
            ),
            process_usage: None,
            subtasks_expanded: false,
            status: SessionStatus::Running,
            status_label: "running",
            is_jump_match: false,
            is_selected: false,
        };
        let palette = Palette::default();
        let line = workspace_row_line(&row, &palette, true, true, false, true, false);

        let detail_span =
            line.spans.iter().find(|span| span.content.as_ref() == "smart").expect("detail span");

        assert_eq!(detail_span.style.fg, Some(Color::Indexed(2)));
    }

    #[test]
    fn workspace_row_renders_output_after_app_detail_and_stats() {
        let row = PaneRow {
            pane_id: "%8".to_string(),
            inactive_since_label: "09:15".to_string(),
            output_excerpt: Some("Final visible output excerpt".to_string().into()),
            client_label: "Amp",
            detail: Some(
                AgentDetail { label: "smart".to_string(), tone: AgentDetailTone::Positive }.into(),
            ),
            process_usage: Some(
                SessionProcessUsage {
                    agent: ResourceUsage { cpu_tenths_percent: 21, memory_kib: 48 * 1024 },
                    spawned: ResourceUsage { cpu_tenths_percent: 0, memory_kib: 3 * 1024 },
                    subtasks: Vec::new(),
                }
                .into(),
            ),
            subtasks_expanded: false,
            status: SessionStatus::Running,
            status_label: "running",
            is_jump_match: false,
            is_selected: false,
        };
        let palette = Palette::default();
        let line = workspace_row_line(&row, &palette, true, true, true, true, true);
        let app_position =
            line.spans.iter().position(|span| span.content.as_ref() == "Amp").expect("app span");
        let detail_position = line
            .spans
            .iter()
            .position(|span| span.content.as_ref() == "smart")
            .expect("detail span");
        let stats_position = line
            .spans
            .iter()
            .position(|span| span.content.as_ref() == "[2.1%/48M]")
            .expect("stats span");
        let output_position = line
            .spans
            .iter()
            .position(|span| span.content.as_ref() == "Final visible output excerpt")
            .expect("output span");

        assert!(app_position < detail_position);
        assert!(detail_position < stats_position);
        assert!(stats_position < output_position);
    }

    #[test]
    fn workspace_row_highlights_jump_matches() {
        let row = PaneRow {
            pane_id: "%19".to_string(),
            inactive_since_label: "09:15".to_string(),
            output_excerpt: None,
            client_label: "Amp",
            detail: None,
            process_usage: None,
            subtasks_expanded: false,
            status: SessionStatus::WaitingInput,
            status_label: "waiting-input",
            is_jump_match: true,
            is_selected: false,
        };
        let palette = Palette::default();
        let line = workspace_row_line(&row, &palette, false, false, false, false, false);
        let pane_cell = "%19";
        let pane_span =
            line.spans.iter().find(|span| span.content.as_ref() == pane_cell).expect("pane span");

        assert_eq!(line.spans[0].content.as_ref(), "●");
        assert_eq!(pane_span.style.fg, Some(Color::Indexed(12)));
        assert!(pane_span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn workspace_header_uses_second_line_for_git_summary_with_semantic_colors() {
        let group = WorkspaceGroup {
            label: "api".to_string(),
            git_summary: Some(GitSummaryRow {
                workspace_path: PathBuf::from("/tmp/api"),
                workspace_label: "api".to_string(),
                branch_name: "main".to_string(),
                insertions: 3,
                deletions: 1,
            }),
            rows: Vec::new(),
        };
        let palette = Palette::default();
        let lines = workspace_header_lines(&group, &palette, true, 120);
        let buffer = Buffer::with_lines(lines.clone());
        let insert_x = "main  ".len() as u16;
        let delete_x = insert_x + "+3 ".len() as u16;
        let header_text =
            lines[0].spans.iter().map(|span| span.content.as_ref()).collect::<String>();

        assert_eq!(lines.len(), 2);
        assert_eq!(header_text.len(), 120);
        assert!(header_text.starts_with("api"));
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Indexed(12)));
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::Indexed(8)));
        assert_eq!(lines[1].spans[2].style.fg, Some(Color::Indexed(2)));
        assert_eq!(lines[1].spans[4].style.fg, Some(Color::Indexed(1)));
        assert_eq!(buffer.cell((insert_x, 1)).expect("insertions cell").fg, Color::Indexed(2));
        assert_eq!(buffer.cell((delete_x, 1)).expect("deletions cell").fg, Color::Indexed(1));
    }

    #[test]
    fn workspace_header_hides_git_summary_when_disabled() {
        let group = WorkspaceGroup {
            label: "api".to_string(),
            git_summary: Some(GitSummaryRow {
                workspace_path: PathBuf::from("/tmp/api"),
                workspace_label: "api".to_string(),
                branch_name: "main".to_string(),
                insertions: 3,
                deletions: 1,
            }),
            rows: Vec::new(),
        };
        let palette = Palette::default();
        let line = workspace_header_line(&group, &palette, 10);
        let text = line.spans.iter().map(|span| span.content.as_ref()).collect::<String>();

        assert_eq!(text.trim_end(), "api");
        assert_eq!(text.len(), 10);
    }

    #[test]
    fn workspace_header_moves_git_stats_to_new_line_when_branch_is_long() {
        let group = WorkspaceGroup {
            label: "api".to_string(),
            git_summary: Some(GitSummaryRow {
                workspace_path: PathBuf::from("/tmp/api"),
                workspace_label: "api".to_string(),
                branch_name: "feature/very-long-branch".to_string(),
                insertions: 3,
                deletions: 1,
            }),
            rows: Vec::new(),
        };
        let palette = Palette::default();
        let lines = workspace_header_lines(&group, &palette, true, 10);
        let buffer = Buffer::with_lines(lines.clone());

        assert_eq!(lines.len(), 3);
        let header_text =
            lines[0].spans.iter().map(|span| span.content.as_ref()).collect::<String>();
        assert_eq!(header_text.len(), 10);
        assert_eq!(header_text.trim_end(), "api");
        assert!(lines[1]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .contains("feature/very-long-branch"));
        assert_eq!(
            lines[2].spans.iter().map(|span| span.content.as_ref()).collect::<String>(),
            "+3 -1"
        );
        assert_eq!(lines[2].spans[0].style.fg, Some(Color::Indexed(2)));
        assert_eq!(lines[2].spans[2].style.fg, Some(Color::Indexed(1)));
        assert_eq!(buffer.cell((0, 2)).expect("insertions cell").fg, Color::Indexed(2));
        assert_eq!(buffer.cell((3, 2)).expect("deletions cell").fg, Color::Indexed(1));
    }

    #[test]
    fn footer_line_matches_raymon_style_shortcut_layout() {
        let palette = Palette::default();
        let key_style = palette.style_for(SemanticRole::HeadingAccent).add_modifier(Modifier::BOLD);
        let sep_style = palette.style_for(SemanticRole::MutedText);
        let line = footer_line(key_style, sep_style);
        let text = line.spans.iter().map(|span| span.content.as_ref()).collect::<String>();

        assert_eq!(
            text,
            "Move:j/k | Pane:% | App:a | Bell:b | Git:g | Model:m | Time:t | Output:o | Stats:s | Subs:= | Jump:Enter | Quit:q"
        );
        assert_eq!(line.spans[0].style.fg, Some(Color::Indexed(8)));
        assert_eq!(line.spans[2].style.fg, Some(Color::Indexed(12)));
        assert_eq!(FOOTER_BRAND, "🅸 🅻 🅼 🅰 🆁 🅸 ");
    }

    #[test]
    fn compact_footer_line_lists_action_keys_only() {
        let palette = Palette::default();
        let key_style = palette.style_for(SemanticRole::HeadingAccent).add_modifier(Modifier::BOLD);
        let sep_style = palette.style_for(SemanticRole::MutedText);
        let line = compact_footer_line(key_style, sep_style);
        let text = line.spans.iter().map(|span| span.content.as_ref()).collect::<String>();

        assert_eq!(text, "j k % a b g m t o s = q");
        assert_eq!(line.spans[0].style.fg, Some(Color::Indexed(12)));
    }

    #[test]
    fn workspace_lines_render_expanded_subtasks_under_parent_row() {
        let palette = Palette::default();
        let lines = super::workspace_lines(
            &AppModel {
                title: "ilmari".to_string(),
                status_line: String::new(),
                show_app: false,
                show_git: true,
                show_detail: true,
                show_time: true,
                show_output: true,
                show_stats: true,
                workspace_groups: vec![WorkspaceGroup {
                    label: "api".to_string(),
                    git_summary: None,
                    rows: vec![PaneRow {
                        pane_id: "%7".to_string(),
                        inactive_since_label: String::new(),
                        output_excerpt: None,
                        client_label: "Codex",
                        detail: None,
                        process_usage: Some(
                            SessionProcessUsage {
                                agent: ResourceUsage {
                                    cpu_tenths_percent: 154,
                                    memory_kib: 64 * 1024,
                                },
                                spawned: ResourceUsage {
                                    cpu_tenths_percent: 8,
                                    memory_kib: 12 * 1024,
                                },
                                subtasks: vec![
                                    SubtaskProcess {
                                        pid: 102,
                                        depth: 0,
                                        command_label: "tmux-mcp-rs".to_string(),
                                        usage: ResourceUsage {
                                            cpu_tenths_percent: 8,
                                            memory_kib: 12 * 1024,
                                        },
                                    },
                                    SubtaskProcess {
                                        pid: 103,
                                        depth: 1,
                                        command_label: "helper".to_string(),
                                        usage: ResourceUsage {
                                            cpu_tenths_percent: 2,
                                            memory_kib: 1024,
                                        },
                                    },
                                ],
                            }
                            .into(),
                        ),
                        subtasks_expanded: true,
                        status: SessionStatus::Running,
                        status_label: "running",
                        is_jump_match: false,
                        is_selected: true,
                    }],
                }],
                refresh_interval: Duration::from_secs(5),
                last_refresh: Instant::now(),
                last_refresh_wallclock: SystemTime::now(),
            },
            &palette,
            120,
        );
        let text = lines
            .iter()
            .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>();

        assert!(text[1].contains("[15.4%/64M]"));
        assert!(text[2].contains("tmux-mcp-rs #102"));
        assert!(text[3].contains("helper #103"));
    }

    #[test]
    fn footer_renders_with_empty_row_before_footer() {
        let model = AppModel {
            title: "ilmari".to_string(),
            status_line: String::new(),
            show_app: false,
            show_git: true,
            show_detail: true,
            show_time: true,
            show_output: true,
            show_stats: false,
            workspace_groups: vec![WorkspaceGroup {
                label: "api".to_string(),
                git_summary: Some(GitSummaryRow {
                    workspace_path: PathBuf::from("/tmp/api"),
                    workspace_label: "api".to_string(),
                    branch_name: "main".to_string(),
                    insertions: 3,
                    deletions: 1,
                }),
                rows: vec![
                    PaneRow {
                        pane_id: "%7".to_string(),
                        inactive_since_label: String::new(),
                        output_excerpt: None,
                        client_label: "Codex",
                        detail: None,
                        process_usage: None,
                        subtasks_expanded: false,
                        status: SessionStatus::Running,
                        status_label: "running",
                        is_jump_match: false,
                        is_selected: true,
                    },
                    PaneRow {
                        pane_id: "%8".to_string(),
                        inactive_since_label: String::new(),
                        output_excerpt: None,
                        client_label: "Codex",
                        detail: None,
                        process_usage: None,
                        subtasks_expanded: false,
                        status: SessionStatus::Running,
                        status_label: "running",
                        is_jump_match: false,
                        is_selected: false,
                    },
                    PaneRow {
                        pane_id: "%9".to_string(),
                        inactive_since_label: String::new(),
                        output_excerpt: None,
                        client_label: "Codex",
                        detail: None,
                        process_usage: None,
                        subtasks_expanded: false,
                        status: SessionStatus::Running,
                        status_label: "running",
                        is_jump_match: false,
                        is_selected: false,
                    },
                    PaneRow {
                        pane_id: "%10".to_string(),
                        inactive_since_label: String::new(),
                        output_excerpt: None,
                        client_label: "Codex",
                        detail: None,
                        process_usage: None,
                        subtasks_expanded: false,
                        status: SessionStatus::Running,
                        status_label: "running",
                        is_jump_match: false,
                        is_selected: false,
                    },
                ],
            }],
            refresh_interval: Duration::from_secs(5),
            last_refresh: Instant::now(),
            last_refresh_wallclock: SystemTime::now(),
        };
        let palette = Palette::default();
        let backend = TestBackend::new(FOOTER_COMPACT_BREAKPOINT - 1, 8);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");

        terminal.draw(|frame| super::render(frame, &model, &palette)).expect("frame should render");

        let buffer = terminal.backend().buffer();

        let footer_row = (0..(FOOTER_COMPACT_BREAKPOINT - 1))
            .map(|x| buffer.cell((x, 7)).expect("footer cell").symbol())
            .collect::<String>();
        let last_main_row = (0..(FOOTER_COMPACT_BREAKPOINT - 1))
            .map(|x| buffer.cell((x, 5)).expect("main cell").symbol())
            .collect::<String>();

        assert!(footer_row.starts_with("j k % a b g m t o s = q"));
        assert!(footer_row.trim_end().ends_with("🅸 🅻 🅼 🅰 🆁 🅸"));
        assert!(last_main_row.contains("%10"));
        assert_eq!(buffer.cell((0, 6)).expect("row above footer").symbol(), " ");
    }

    #[test]
    fn empty_status_line_does_not_reserve_leading_rows() {
        let model = AppModel {
            title: "ilmari".to_string(),
            status_line: String::new(),
            show_app: false,
            show_git: true,
            show_detail: false,
            show_time: true,
            show_output: true,
            show_stats: false,
            workspace_groups: vec![WorkspaceGroup {
                label: "api".to_string(),
                git_summary: Some(GitSummaryRow {
                    workspace_path: PathBuf::from("/tmp/api"),
                    workspace_label: "api".to_string(),
                    branch_name: "main".to_string(),
                    insertions: 3,
                    deletions: 1,
                }),
                rows: vec![PaneRow {
                    pane_id: "%7".to_string(),
                    inactive_since_label: String::new(),
                    output_excerpt: None,
                    client_label: "Codex",
                    detail: None,
                    process_usage: None,
                    subtasks_expanded: false,
                    status: SessionStatus::Running,
                    status_label: "running",
                    is_jump_match: false,
                    is_selected: true,
                }],
            }],
            refresh_interval: Duration::from_secs(5),
            last_refresh: Instant::now(),
            last_refresh_wallclock: SystemTime::now(),
        };
        let palette = Palette::default();
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");

        terminal.draw(|frame| super::render(frame, &model, &palette)).expect("frame should render");

        let buffer = terminal.backend().buffer();

        assert_eq!(buffer.cell((0, 0)).expect("workspace label cell").symbol(), "a");
        assert_eq!(buffer.cell((0, 1)).expect("git summary cell").symbol(), "m");
    }

    #[test]
    fn workspace_row_hides_model_and_stats_when_disabled() {
        let row = PaneRow {
            pane_id: "%7".to_string(),
            inactive_since_label: String::new(),
            output_excerpt: Some(
                "Could you clarify what you'd like me to help with?".to_string().into(),
            ),
            client_label: "Codex",
            detail: Some(
                AgentDetail {
                    label: "gpt-5.4 xhigh fast".to_string(),
                    tone: AgentDetailTone::Neutral,
                }
                .into(),
            ),
            process_usage: Some(
                SessionProcessUsage {
                    agent: ResourceUsage { cpu_tenths_percent: 154, memory_kib: 64 * 1024 },
                    spawned: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 12 * 1024 },
                    subtasks: vec![SubtaskProcess {
                        pid: 102,
                        depth: 0,
                        command_label: "tmux-mcp-rs".to_string(),
                        usage: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 12 * 1024 },
                    }],
                }
                .into(),
            ),
            subtasks_expanded: false,
            status: SessionStatus::Running,
            status_label: "running",
            is_jump_match: false,
            is_selected: false,
        };
        let palette = Palette::default();
        let line = workspace_row_line(&row, &palette, false, false, false, false, false);
        let text = line.spans.iter().map(|span| span.content.as_ref()).collect::<String>();

        assert!(!text.contains("gpt-5.4 xhigh fast"));
        assert!(!text.contains("Could you clarify what you'd like me to help with?"));
        assert!(!text.contains("Codex"));
        assert!(!text.contains("[15.4%/64M]"));
        assert!(!text.contains("[0.8%/12M]"));
        assert!(text.contains("▶"));
    }
}
