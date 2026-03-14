use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use thiserror::Error;

use crate::agents::SessionTracker;

pub const LIST_PANES_FORMAT: &str = "#{pane_id}\t#{pane_pid}\t#{session_id}\t#{session_name}\t#{window_id}\t#{window_name}\t#{pane_dead}\t#{pane_current_path}\t#{pane_current_command}\t#{pane_title}";
pub const DEFAULT_CAPTURE_START: &str = "-80";
const PANE_SNAPSHOT_FIELD_COUNT: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSnapshot {
    pub pane_id: String,
    pub pane_pid: Option<u32>,
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_dead: bool,
    pub pane_current_path: PathBuf,
    pub pane_current_command: String,
    pub pane_title: String,
}

impl PaneSnapshot {
    pub fn parse(line: &str) -> Result<Self, PaneSnapshotParseError> {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != PANE_SNAPSHOT_FIELD_COUNT {
            return Err(PaneSnapshotParseError::InvalidFieldCount {
                expected: PANE_SNAPSHOT_FIELD_COUNT,
                actual: fields.len(),
            });
        }

        Ok(Self {
            pane_id: parse_required_field(fields[0], "pane_id")?.to_string(),
            pane_pid: parse_optional_u32(fields[1], "pane_pid")?,
            session_id: parse_required_field(fields[2], "session_id")?.to_string(),
            session_name: parse_required_field(fields[3], "session_name")?.to_string(),
            window_id: parse_required_field(fields[4], "window_id")?.to_string(),
            window_name: parse_required_field(fields[5], "window_name")?.to_string(),
            pane_dead: parse_bool_flag(fields[6], "pane_dead")?,
            pane_current_path: PathBuf::from(fields[7]),
            pane_current_command: fields[8].to_string(),
            pane_title: fields[9].to_string(),
        })
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PaneSnapshotParseError {
    #[error("expected {expected} tab-separated fields, got {actual}")]
    InvalidFieldCount { expected: usize, actual: usize },
    #[error("missing required field `{field}`")]
    MissingField { field: &'static str },
    #[error("invalid unsigned integer for `{field}`: `{value}`")]
    InvalidUnsignedInteger { field: &'static str, value: String },
    #[error("invalid boolean flag for `{field}`: `{value}`")]
    InvalidBooleanFlag { field: &'static str, value: String },
}

fn parse_required_field<'a>(
    value: &'a str,
    field: &'static str,
) -> Result<&'a str, PaneSnapshotParseError> {
    if value.is_empty() {
        return Err(PaneSnapshotParseError::MissingField { field });
    }

    Ok(value)
}

fn parse_optional_u32(
    value: &str,
    field: &'static str,
) -> Result<Option<u32>, PaneSnapshotParseError> {
    if value.is_empty() {
        return Ok(None);
    }

    value.parse::<u32>().map(Some).map_err(|_| PaneSnapshotParseError::InvalidUnsignedInteger {
        field,
        value: value.to_string(),
    })
}

fn parse_bool_flag(value: &str, field: &'static str) -> Result<bool, PaneSnapshotParseError> {
    match value {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        _ => Err(PaneSnapshotParseError::InvalidBooleanFlag { field, value: value.to_string() }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxCommand {
    args: Vec<String>,
}

impl TmuxCommand {
    pub fn new(args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self { args: args.into_iter().map(Into::into).collect() }
    }

    #[cfg(test)]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    fn as_command(&self) -> Command {
        let mut command = Command::new("tmux");
        command.args(&self.args);
        command
    }

    fn render(&self) -> String {
        format!("tmux {}", self.args.join(" "))
    }
}

#[derive(Debug, Error)]
pub enum TmuxError {
    #[error("failed to execute tmux: {0}")]
    Io(#[from] io::Error),
    #[error("tmux output was not valid utf-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("tmux command failed: {command} (exit code: {exit_code:?}) {stderr}")]
    CommandFailed { command: String, exit_code: Option<i32>, stderr: String },
    #[error("failed to parse tmux pane snapshot on line {line_number}: {source}")]
    ParseSnapshotLine {
        line_number: usize,
        #[source]
        source: PaneSnapshotParseError,
    },
}

pub fn pane_snapshot_command() -> TmuxCommand {
    TmuxCommand::new(["list-panes", "-aF", LIST_PANES_FORMAT])
}

pub fn collect_pane_snapshots() -> Result<Vec<PaneSnapshot>, TmuxError> {
    let stdout = run_tmux_command(&pane_snapshot_command())?;
    parse_pane_snapshots(&stdout)
}

pub fn parse_pane_snapshots(stdout: &str) -> Result<Vec<PaneSnapshot>, TmuxError> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            PaneSnapshot::parse(line)
                .map_err(|source| TmuxError::ParseSnapshotLine { line_number: index + 1, source })
        })
        .collect()
}

pub fn capture_output_tail_command(target: &str, start: &str) -> TmuxCommand {
    TmuxCommand::new(["capture-pane", "-p", "-J", "-t", target, "-S", start])
}

pub fn capture_output_tail(target: &str, start: &str) -> Result<String, TmuxError> {
    run_tmux_command(&capture_output_tail_command(target, start))
}

pub fn capture_output_tails(
    panes: &[PaneSnapshot],
    tracker: &SessionTracker,
    _now: Instant,
) -> HashMap<String, String> {
    let previous = tracker.records();

    panes
        .iter()
        .filter_map(|pane| {
            let previous = previous.get(&pane.pane_id);
            if !tracker.registry().needs_output_tail(pane, previous) {
                return None;
            }

            capture_output_tail(&pane.pane_id, DEFAULT_CAPTURE_START)
                .ok()
                .map(|output_tail| (pane.pane_id.clone(), output_tail))
        })
        .collect()
}

pub fn jump_command(target: &PaneSnapshot) -> TmuxCommand {
    TmuxCommand::new([
        "switch-client",
        "-t",
        target.session_id.as_str(),
        ";",
        "select-window",
        "-t",
        target.window_id.as_str(),
        ";",
        "select-pane",
        "-t",
        target.pane_id.as_str(),
    ])
}

pub fn jump_to_pane(target: &PaneSnapshot) -> Result<(), TmuxError> {
    run_tmux_command(&jump_command(target))?;
    Ok(())
}

fn run_tmux_command(command: &TmuxCommand) -> Result<String, TmuxError> {
    let output = command.as_command().output()?;
    if !output.status.success() {
        return Err(TmuxError::CommandFailed {
            command: command.render(),
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    Ok(String::from_utf8(output.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::{
        capture_output_tail_command, jump_command, pane_snapshot_command, parse_pane_snapshots,
        PaneSnapshot, PaneSnapshotParseError, DEFAULT_CAPTURE_START, LIST_PANES_FORMAT,
    };
    use std::path::PathBuf;

    #[test]
    fn snapshot_command_uses_global_tab_separated_format() {
        let command = pane_snapshot_command();

        assert_eq!(
            command.args(),
            &["list-panes".to_string(), "-aF".to_string(), LIST_PANES_FORMAT.to_string(),]
        );
    }

    #[test]
    fn parse_pane_snapshots_reads_multiple_rows() {
        let snapshots = parse_pane_snapshots(
            "%1\t101\t$1\twork\t@1\teditor\t0\t/tmp/api\tcodex\tagent\n%9\t202\t$2\tops\t@3\tlogs\t1\t/tmp/blog\tamp\treview\n",
        )
        .expect("tmux pane output should parse");

        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].pane_id, "%1");
        assert_eq!(snapshots[0].pane_pid, Some(101));
        assert_eq!(snapshots[0].session_id, "$1");
        assert_eq!(snapshots[0].pane_current_command, "codex");
        assert_eq!(snapshots[1].pane_id, "%9");
        assert!(snapshots[1].pane_dead);
        assert_eq!(snapshots[1].pane_current_command, "amp");
    }

    #[test]
    fn parse_pane_snapshots_reports_the_failing_line() {
        let error = parse_pane_snapshots(
            "%1\t101\t$1\twork\t@1\teditor\t0\t/tmp/api\tcodex\tagent\n%9\t202\t$2\tops\t@3\tlogs\tmaybe\t/tmp/blog\tamp\treview\n",
        )
        .expect_err("invalid pane_dead flag should fail");

        match error {
            super::TmuxError::ParseSnapshotLine {
                line_number,
                source: PaneSnapshotParseError::InvalidBooleanFlag { field, value },
            } => {
                assert_eq!(line_number, 2);
                assert_eq!(field, "pane_dead");
                assert_eq!(value, "maybe");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn jump_command_targets_stable_tmux_ids() {
        let target = PaneSnapshot::parse(
            "%12\t301\t$5\tclient\t@8\tagents\t0\t/Users/bnomei/Sites/ilmari\tcodex\tworker",
        )
        .expect("pane snapshot should parse");

        assert_eq!(
            jump_command(&target).args(),
            &[
                "switch-client".to_string(),
                "-t".to_string(),
                "$5".to_string(),
                ";".to_string(),
                "select-window".to_string(),
                "-t".to_string(),
                "@8".to_string(),
                ";".to_string(),
                "select-pane".to_string(),
                "-t".to_string(),
                "%12".to_string(),
            ]
        );
    }

    #[test]
    fn capture_output_tail_command_joins_wrapped_lines_from_default_tail_window() {
        assert_eq!(
            capture_output_tail_command("%12", DEFAULT_CAPTURE_START).args(),
            &[
                "capture-pane".to_string(),
                "-p".to_string(),
                "-J".to_string(),
                "-t".to_string(),
                "%12".to_string(),
                "-S".to_string(),
                DEFAULT_CAPTURE_START.to_string(),
            ]
        );
    }

    #[test]
    fn pane_snapshot_parser_accepts_blank_optional_fields() {
        let snapshot =
            PaneSnapshot::parse("%12\t\t$1\tdev\t@3\teditor\t0\t/Users/bnomei/Sites/ilmari\t\t")
                .expect("snapshot should parse");

        assert_eq!(snapshot.pane_id, "%12");
        assert_eq!(snapshot.pane_pid, None);
        assert_eq!(snapshot.session_id, "$1");
        assert_eq!(snapshot.window_id, "@3");
        assert!(!snapshot.pane_dead);
        assert_eq!(snapshot.pane_current_path, PathBuf::from("/Users/bnomei/Sites/ilmari"));
        assert_eq!(snapshot.pane_current_command, "");
        assert_eq!(snapshot.pane_title, "");
    }

    #[test]
    fn pane_snapshot_parser_rejects_bad_dead_flag() {
        let error = PaneSnapshot::parse(
            "%12\t123\t$1\tdev\t@3\teditor\tnope\t/Users/bnomei/Sites/ilmari\tcodex\ttitle",
        )
        .expect_err("invalid pane_dead flag should fail");

        assert_eq!(
            error,
            PaneSnapshotParseError::InvalidBooleanFlag {
                field: "pane_dead",
                value: "nope".to_string(),
            }
        );
    }
}
