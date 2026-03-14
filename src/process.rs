use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::process::Command;

use thiserror::Error;

use crate::model::{AgentKind, ResourceUsage, SessionProcessUsage, SessionRecord, SubtaskProcess};

const PS_FORMAT: &str = "pid=,ppid=,%cpu=,rss=,comm=";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub ppid: u32,
    pub cpu_tenths_percent: u32,
    pub memory_kib: u64,
    pub command: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProcessSnapshotParseError {
    #[error("missing required field `{field}`")]
    MissingField { field: &'static str },
    #[error("invalid unsigned integer for `{field}`: `{value}`")]
    InvalidUnsignedInteger { field: &'static str, value: String },
    #[error("invalid cpu percentage for `{field}`: `{value}`")]
    InvalidCpuPercent { field: &'static str, value: String },
}

impl ProcessSnapshot {
    pub fn parse(line: &str) -> Result<Self, ProcessSnapshotParseError> {
        let (pid, rest) = next_field(line, "pid")?;
        let (ppid, rest) = next_field(rest, "ppid")?;
        let (cpu_percent, rest) = next_field(rest, "%cpu")?;
        let (memory_kib, rest) = next_field(rest, "rss")?;
        let command = rest.trim_start();

        if command.is_empty() {
            return Err(ProcessSnapshotParseError::MissingField { field: "comm" });
        }

        Ok(Self {
            pid: parse_u32(pid, "pid")?,
            ppid: parse_u32(ppid, "ppid")?,
            cpu_tenths_percent: parse_cpu_tenths(cpu_percent, "%cpu")?,
            memory_kib: parse_u64(memory_kib, "rss")?,
            command: command.to_string(),
        })
    }
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("failed to execute ps: {0}")]
    Io(#[from] io::Error),
    #[error("ps output was not valid utf-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("ps command failed: {command} (exit code: {exit_code:?}) {stderr}")]
    CommandFailed { command: String, exit_code: Option<i32>, stderr: String },
    #[error("failed to parse ps snapshot on line {line_number}: {source}")]
    ParseSnapshotLine {
        line_number: usize,
        #[source]
        source: ProcessSnapshotParseError,
    },
}

#[derive(Debug, Clone)]
pub struct ProcessTree {
    processes: HashMap<u32, ProcessSnapshot>,
    children: HashMap<u32, Vec<u32>>,
}

impl ProcessTree {
    pub fn from_snapshots(snapshots: Vec<ProcessSnapshot>) -> Self {
        let mut processes = HashMap::with_capacity(snapshots.len());
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();

        for snapshot in snapshots {
            children.entry(snapshot.ppid).or_default().push(snapshot.pid);
            processes.insert(snapshot.pid, snapshot);
        }

        for child_pids in children.values_mut() {
            child_pids.sort_unstable();
        }

        Self { processes, children }
    }

    pub fn usage_for_session(&self, session: &SessionRecord) -> Option<SessionProcessUsage> {
        self.usage_for_kind(session.pane.pane_pid, session.kind)
    }

    fn usage_for_kind(
        &self,
        pane_pid: Option<u32>,
        kind: AgentKind,
    ) -> Option<SessionProcessUsage> {
        let pane_pid = pane_pid?;
        let agent_pid = self.resolve_agent_pid(pane_pid, kind)?;

        let subtasks = self.collect_descendants(agent_pid);
        let spawned = subtasks
            .iter()
            .fold(ResourceUsage::zero(), |total, subtask| total.saturating_add(subtask.usage));

        Some(SessionProcessUsage { agent: self.resource_usage(agent_pid)?, spawned, subtasks })
    }

    fn resolve_agent_pid(&self, pane_pid: u32, kind: AgentKind) -> Option<u32> {
        if self.process_matches_kind(pane_pid, kind) {
            return Some(pane_pid);
        }

        let mut queue: VecDeque<u32> =
            self.children.get(&pane_pid).cloned().unwrap_or_default().into();
        while let Some(pid) = queue.pop_front() {
            if self.process_matches_kind(pid, kind) {
                return Some(pid);
            }

            if let Some(children) = self.children.get(&pid) {
                queue.extend(children.iter().copied());
            }
        }

        None
    }

    fn process_matches_kind(&self, pid: u32, kind: AgentKind) -> bool {
        let Some(process) = self.processes.get(&pid) else {
            return false;
        };

        match kind {
            AgentKind::Codex => command_matches(&process.command, "codex"),
            AgentKind::Amp => command_matches(&process.command, "amp"),
            AgentKind::ClaudeCode => {
                command_equals_any(&process.command, &["claude", "claude-code"])
            }
            AgentKind::OpenCode => command_matches(&process.command, "opencode"),
            AgentKind::Pi => command_equals_any(&process.command, &["pi", "pi-agent"]),
        }
    }

    fn resource_usage(&self, pid: u32) -> Option<ResourceUsage> {
        self.processes.get(&pid).map(|process| ResourceUsage {
            cpu_tenths_percent: process.cpu_tenths_percent,
            memory_kib: process.memory_kib,
        })
    }

    fn collect_descendants(&self, root_pid: u32) -> Vec<SubtaskProcess> {
        let mut subtasks = Vec::new();
        let mut queue: VecDeque<(u32, usize)> = self
            .children
            .get(&root_pid)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|pid| (pid, 0))
            .collect();
        let mut visited = HashSet::new();

        while let Some((pid, depth)) = queue.pop_front() {
            if !visited.insert(pid) {
                continue;
            }

            if let (Some(process), Some(usage)) =
                (self.processes.get(&pid), self.resource_usage(pid))
            {
                subtasks.push(SubtaskProcess {
                    pid,
                    depth,
                    command_label: display_command_label(&process.command),
                    usage,
                });
            }

            if let Some(children) = self.children.get(&pid) {
                queue.extend(children.iter().copied().map(|child| (child, depth + 1)));
            }
        }

        subtasks
    }
}

pub fn collect_process_tree() -> Result<ProcessTree, ProcessError> {
    Ok(ProcessTree::from_snapshots(collect_process_snapshots()?))
}

pub fn collect_process_snapshots() -> Result<Vec<ProcessSnapshot>, ProcessError> {
    let output = Command::new("/bin/ps").args(["-axo", PS_FORMAT]).output()?;
    if !output.status.success() {
        return Err(ProcessError::CommandFailed {
            command: format!("/bin/ps -axo {PS_FORMAT}"),
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    parse_process_snapshots(&String::from_utf8(output.stdout)?)
}

pub fn parse_process_snapshots(stdout: &str) -> Result<Vec<ProcessSnapshot>, ProcessError> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            ProcessSnapshot::parse(line).map_err(|source| ProcessError::ParseSnapshotLine {
                line_number: index + 1,
                source,
            })
        })
        .collect()
}

fn next_field<'a>(
    input: &'a str,
    field: &'static str,
) -> Result<(&'a str, &'a str), ProcessSnapshotParseError> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return Err(ProcessSnapshotParseError::MissingField { field });
    }

    let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    Ok(trimmed.split_at(end))
}

fn parse_u32(value: &str, field: &'static str) -> Result<u32, ProcessSnapshotParseError> {
    value.parse::<u32>().map_err(|_| ProcessSnapshotParseError::InvalidUnsignedInteger {
        field,
        value: value.to_string(),
    })
}

fn parse_u64(value: &str, field: &'static str) -> Result<u64, ProcessSnapshotParseError> {
    value.parse::<u64>().map_err(|_| ProcessSnapshotParseError::InvalidUnsignedInteger {
        field,
        value: value.to_string(),
    })
}

fn parse_cpu_tenths(value: &str, field: &'static str) -> Result<u32, ProcessSnapshotParseError> {
    let parsed = value.parse::<f32>().map_err(|_| {
        ProcessSnapshotParseError::InvalidCpuPercent { field, value: value.to_string() }
    })?;
    if !parsed.is_finite() || parsed < 0.0 {
        return Err(ProcessSnapshotParseError::InvalidCpuPercent {
            field,
            value: value.to_string(),
        });
    }

    Ok((parsed * 10.0).round() as u32)
}

fn command_matches(command: &str, expected: &str) -> bool {
    let normalized = normalized_command_name(command);
    normalized == expected || normalized.starts_with(&format!("{expected}-"))
}

fn command_equals_any(command: &str, expected: &[&str]) -> bool {
    let normalized = normalized_command_name(command);
    expected.iter().any(|candidate| normalized == *candidate)
}

fn normalized_command_name(command: &str) -> String {
    command
        .trim()
        .rsplit(|character: char| character == '/' || character.is_whitespace())
        .find(|segment| !segment.is_empty())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn display_command_label(command: &str) -> String {
    command
        .trim()
        .rsplit(|character: char| character == '/' || character.is_whitespace())
        .find(|segment| !segment.is_empty())
        .unwrap_or(command.trim())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{parse_process_snapshots, ProcessSnapshot, ProcessTree};
    use crate::model::{
        AgentKind, ResourceUsage, SessionProcessUsage, SessionRecord, SessionStatus, SubtaskProcess,
    };
    use crate::tmux::PaneSnapshot;
    use std::time::Instant;

    #[test]
    fn process_snapshot_parser_keeps_command_paths_with_spaces() {
        let snapshot = ProcessSnapshot::parse("101 55 12.4 4096 /Users/test/My Tools/codex")
            .expect("process snapshot should parse");

        assert_eq!(snapshot.pid, 101);
        assert_eq!(snapshot.ppid, 55);
        assert_eq!(snapshot.cpu_tenths_percent, 124);
        assert_eq!(snapshot.memory_kib, 4096);
        assert_eq!(snapshot.command, "/Users/test/My Tools/codex");
    }

    #[test]
    fn process_tree_splits_agent_and_spawned_usage() {
        let tree = ProcessTree::from_snapshots(vec![
            snapshot(101, 55, 321, 60 * 1024, "codex"),
            snapshot(102, 101, 12, 8 * 1024, "/tmp/tmux-mcp-rs"),
            snapshot(103, 102, 8, 2 * 1024, "helper"),
        ]);

        let usage = tree.usage_for_kind(Some(101), AgentKind::Codex).expect("usage should resolve");

        assert_eq!(
            usage,
            SessionProcessUsage {
                agent: ResourceUsage { cpu_tenths_percent: 321, memory_kib: 60 * 1024 },
                spawned: ResourceUsage { cpu_tenths_percent: 20, memory_kib: 10 * 1024 },
                subtasks: vec![
                    SubtaskProcess {
                        pid: 102,
                        depth: 0,
                        command_label: "tmux-mcp-rs".to_string(),
                        usage: ResourceUsage { cpu_tenths_percent: 12, memory_kib: 8 * 1024 },
                    },
                    SubtaskProcess {
                        pid: 103,
                        depth: 1,
                        command_label: "helper".to_string(),
                        usage: ResourceUsage { cpu_tenths_percent: 8, memory_kib: 2 * 1024 },
                    },
                ],
            }
        );
    }

    #[test]
    fn process_tree_finds_agent_below_shell_root() {
        let tree = ProcessTree::from_snapshots(vec![
            snapshot(100, 55, 1, 1024, "zsh"),
            snapshot(101, 100, 250, 70 * 1024, "codex"),
            snapshot(102, 101, 5, 3 * 1024, "/tmp/tmux-mcp-rs"),
        ]);

        let usage = tree
            .usage_for_session(&session_record(100, AgentKind::Codex))
            .expect("usage should resolve through shell parent");

        assert_eq!(
            usage,
            SessionProcessUsage {
                agent: ResourceUsage { cpu_tenths_percent: 250, memory_kib: 70 * 1024 },
                spawned: ResourceUsage { cpu_tenths_percent: 5, memory_kib: 3 * 1024 },
                subtasks: vec![SubtaskProcess {
                    pid: 102,
                    depth: 0,
                    command_label: "tmux-mcp-rs".to_string(),
                    usage: ResourceUsage { cpu_tenths_percent: 5, memory_kib: 3 * 1024 },
                }],
            }
        );
    }

    #[test]
    fn process_tree_returns_none_when_agent_process_is_gone() {
        let tree = ProcessTree::from_snapshots(vec![
            snapshot(100, 55, 1, 1024, "zsh"),
            snapshot(102, 100, 5, 3 * 1024, "/tmp/tmux-mcp-rs"),
        ]);

        assert!(tree.usage_for_session(&session_record(100, AgentKind::Codex)).is_none());
    }

    #[test]
    fn parse_process_snapshots_reports_line_numbers() {
        let error = parse_process_snapshots("101 1 0.0 1024 codex\nbad line")
            .expect_err("parse should fail");

        assert!(matches!(error, super::ProcessError::ParseSnapshotLine { line_number: 2, .. }));
    }

    fn snapshot(
        pid: u32,
        ppid: u32,
        cpu_tenths_percent: u32,
        memory_kib: u64,
        command: &str,
    ) -> ProcessSnapshot {
        ProcessSnapshot { pid, ppid, cpu_tenths_percent, memory_kib, command: command.to_string() }
    }

    fn session_record(pane_pid: u32, kind: AgentKind) -> SessionRecord {
        let now = Instant::now();

        SessionRecord {
            pane: PaneSnapshot::parse(&format!(
                "%7\t{pane_pid}\t$1\tdev\t@7\tagents\t0\t/Users/bnomei/Sites/ilmari\tzsh\ttitle"
            ))
            .expect("pane snapshot should parse"),
            kind,
            status: SessionStatus::Running,
            detail: None,
            output_excerpt: None,
            process_usage: None,
            output_fingerprint: None,
            last_changed_at: now,
            last_seen_at: now,
            retained_until: None,
        }
    }
}
