use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::model::GitSummaryRow;

pub const DEFAULT_GIT_REFRESH: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitSummaryReport {
    pub rows: Vec<GitSummaryRow>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShortStat {
    pub insertions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone)]
struct CachedRepoRoot {
    value: Option<PathBuf>,
    checked_at: Instant,
}

#[derive(Debug, Clone)]
struct CachedSummary {
    row: GitSummaryRow,
    refreshed_at: Instant,
}

#[derive(Debug)]
pub struct GitSummaryCache {
    refresh_interval: Duration,
    repo_roots: HashMap<PathBuf, CachedRepoRoot>,
    summaries: HashMap<PathBuf, CachedSummary>,
}

impl Default for GitSummaryCache {
    fn default() -> Self {
        Self::new()
    }
}

impl GitSummaryCache {
    pub fn new() -> Self {
        Self::with_refresh_interval(DEFAULT_GIT_REFRESH)
    }

    pub fn with_refresh_interval(refresh_interval: Duration) -> Self {
        Self { refresh_interval, repo_roots: HashMap::new(), summaries: HashMap::new() }
    }

    pub fn summary_rows_for_workspaces<'a, I>(
        &mut self,
        workspace_paths: I,
        now: Instant,
        force_refresh: bool,
    ) -> GitSummaryReport
    where
        I: IntoIterator<Item = &'a Path>,
    {
        let mut active_workspace_paths = HashSet::new();
        let mut unique_roots = Vec::new();
        let mut seen_roots = HashSet::new();
        let mut warnings = Vec::new();

        for workspace_path in workspace_paths {
            let workspace_path = workspace_path.to_path_buf();
            if !active_workspace_paths.insert(workspace_path.clone()) {
                continue;
            }

            let repo_root = match self.resolve_repo_root(&workspace_path, now, force_refresh) {
                Ok(Some(repo_root)) => repo_root,
                Ok(None) => continue,
                Err(error) => {
                    warnings.push(error.to_string());
                    continue;
                }
            };

            if seen_roots.insert(repo_root.clone()) {
                unique_roots.push(repo_root);
            }
        }

        self.repo_roots.retain(|path, _| active_workspace_paths.contains(path));
        self.summaries.retain(|path, _| seen_roots.contains(path));

        let mut rows = Vec::new();
        for repo_root in unique_roots {
            match self.summary_row_for_repo(&repo_root, now, force_refresh) {
                Ok(row) => rows.push(row),
                Err(error) => warnings.push(error.to_string()),
            }
        }

        rows.sort_by(|left, right| left.workspace_label.cmp(&right.workspace_label));
        GitSummaryReport { rows, warnings }
    }

    fn resolve_repo_root(
        &mut self,
        workspace_path: &Path,
        now: Instant,
        force_refresh: bool,
    ) -> Result<Option<PathBuf>, GitError> {
        if !force_refresh {
            if let Some(cached) = self.repo_roots.get(workspace_path) {
                if now.duration_since(cached.checked_at) < self.refresh_interval {
                    return Ok(cached.value.clone());
                }
            }
        }

        let value = resolve_repo_root(workspace_path)?;
        self.repo_roots.insert(
            workspace_path.to_path_buf(),
            CachedRepoRoot { value: value.clone(), checked_at: now },
        );

        Ok(value)
    }

    fn summary_row_for_repo(
        &mut self,
        repo_root: &Path,
        now: Instant,
        force_refresh: bool,
    ) -> Result<GitSummaryRow, GitError> {
        if !force_refresh {
            if let Some(cached) = self.summaries.get(repo_root) {
                if now.duration_since(cached.refreshed_at) < self.refresh_interval {
                    return Ok(cached.row.clone());
                }
            }
        }

        let shortstat = repo_shortstat(repo_root)?;
        let branch_name = repo_branch_name(repo_root)?;
        let row = GitSummaryRow {
            workspace_path: repo_root.to_path_buf(),
            workspace_label: path_label(repo_root),
            branch_name,
            insertions: shortstat.insertions,
            deletions: shortstat.deletions,
        };

        self.summaries
            .insert(repo_root.to_path_buf(), CachedSummary { row: row.clone(), refreshed_at: now });

        Ok(row)
    }
}

#[derive(Debug, Error)]
pub enum GitError {
    #[error("failed to execute git: {0}")]
    Io(#[from] std::io::Error),
    #[error("git output was not valid utf-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("git command failed: {command} (exit code: {exit_code:?}) {stderr}")]
    CommandFailed { command: String, exit_code: Option<i32>, stderr: String },
    #[error("invalid shortstat count in segment `{segment}`")]
    InvalidShortStatCount { segment: String },
}

fn resolve_repo_root(workspace_path: &Path) -> Result<Option<PathBuf>, GitError> {
    let output = run_git(workspace_path, &["rev-parse", "--show-toplevel"])?;
    if output.status.success() {
        let stdout = String::from_utf8(output.stdout)?;
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        return Ok(Some(PathBuf::from(trimmed)));
    }

    if is_not_git_repository(&output) {
        return Ok(None);
    }

    Err(command_failed(workspace_path, &["rev-parse", "--show-toplevel"], output))
}

fn repo_shortstat(repo_root: &Path) -> Result<ShortStat, GitError> {
    let primary = run_git(repo_root, &["diff", "HEAD", "--shortstat"])?;
    if primary.status.success() {
        return parse_shortstat(&String::from_utf8(primary.stdout)?);
    }

    if !is_missing_head_revision(&primary) {
        return Err(command_failed(repo_root, &["diff", "HEAD", "--shortstat"], primary));
    }

    let unstaged = run_git(repo_root, &["diff", "--shortstat"])?;
    if !unstaged.status.success() {
        return Err(command_failed(repo_root, &["diff", "--shortstat"], unstaged));
    }

    let staged = run_git(repo_root, &["diff", "--cached", "--shortstat"])?;
    if !staged.status.success() {
        return Err(command_failed(repo_root, &["diff", "--cached", "--shortstat"], staged));
    }

    let mut combined = parse_shortstat(&String::from_utf8(unstaged.stdout)?)?;
    let staged = parse_shortstat(&String::from_utf8(staged.stdout)?)?;
    combined.insertions += staged.insertions;
    combined.deletions += staged.deletions;
    Ok(combined)
}

fn repo_branch_name(repo_root: &Path) -> Result<String, GitError> {
    let current = run_git(repo_root, &["branch", "--show-current"])?;
    if !current.status.success() {
        return Err(command_failed(repo_root, &["branch", "--show-current"], current));
    }

    let branch_name = String::from_utf8(current.stdout)?.trim().to_string();
    if !branch_name.is_empty() {
        return Ok(branch_name);
    }

    let detached = run_git(repo_root, &["rev-parse", "--short", "HEAD"])?;
    if !detached.status.success() {
        return Err(command_failed(repo_root, &["rev-parse", "--short", "HEAD"], detached));
    }

    let short_head = String::from_utf8(detached.stdout)?.trim().to_string();
    if short_head.is_empty() {
        return Ok("detached".to_string());
    }

    Ok(format!("detached@{short_head}"))
}

pub fn parse_shortstat(output: &str) -> Result<ShortStat, GitError> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Ok(ShortStat::default());
    }

    Ok(ShortStat {
        insertions: extract_count(trimmed, "insertion")?,
        deletions: extract_count(trimmed, "deletion")?,
    })
}

fn extract_count(output: &str, needle: &str) -> Result<u32, GitError> {
    output
        .split(',')
        .find(|segment| segment.contains(needle))
        .map(|segment| {
            segment
                .split_whitespace()
                .next()
                .ok_or_else(|| GitError::InvalidShortStatCount {
                    segment: segment.trim().to_string(),
                })?
                .parse::<u32>()
                .map_err(|_| GitError::InvalidShortStatCount {
                    segment: segment.trim().to_string(),
                })
        })
        .transpose()
        .map(|count| count.unwrap_or(0))
}

fn run_git(path: &Path, args: &[&str]) -> Result<Output, GitError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(path);
    command.args(args);
    Ok(command.output()?)
}

fn command_failed(path: &Path, args: &[&str], output: Output) -> GitError {
    GitError::CommandFailed {
        command: render_git_command(path, args),
        exit_code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    }
}

fn render_git_command(path: &Path, args: &[&str]) -> String {
    let mut command = vec!["git".to_string(), "-C".to_string(), path.display().to_string()];
    command.extend(args.iter().map(|arg| (*arg).to_string()));
    command.join(" ")
}

fn is_not_git_repository(output: &Output) -> bool {
    String::from_utf8_lossy(&output.stderr).to_ascii_lowercase().contains("not a git repository")
}

fn is_missing_head_revision(output: &Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    stderr.contains("bad revision 'head'")
        || stderr.contains("ambiguous argument 'head'")
        || stderr.contains("unknown revision or path not in the working tree")
}

fn path_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::{parse_shortstat, GitSummaryCache, ShortStat};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[test]
    fn parse_shortstat_extracts_insertions_and_deletions() {
        let shortstat = parse_shortstat(" 1 file changed, 3 insertions(+), 1 deletion(-)")
            .expect("shortstat should parse");

        assert_eq!(shortstat, ShortStat { insertions: 3, deletions: 1 });
    }

    #[test]
    fn cache_deduplicates_repo_roots_and_reuses_cached_summaries() {
        let repo = TempDir::new("git-cache-dedup");
        init_repo_with_head(repo.path());

        let tracked = repo.path().join("tracked.txt");
        fs::write(&tracked, "alpha\nbeta\n").expect("tracked file should update");
        let nested = repo.path().join("nested");
        fs::create_dir_all(&nested).expect("nested dir should exist");

        let mut cache = GitSummaryCache::with_refresh_interval(Duration::from_secs(60));
        let now = Instant::now();

        let initial =
            cache.summary_rows_for_workspaces([repo.path(), nested.as_path()], now, false);
        assert!(initial.warnings.is_empty());
        assert_eq!(initial.rows.len(), 1);
        assert_eq!(initial.rows[0].branch_name, "main");
        assert_eq!(initial.rows[0].insertions, 1);
        assert_eq!(initial.rows[0].deletions, 0);

        fs::write(&tracked, "alpha\nbeta\ngamma\n").expect("tracked file should update again");

        let cached =
            cache.summary_rows_for_workspaces([repo.path()], now + Duration::from_secs(5), false);
        assert_eq!(cached.rows[0].insertions, 1);

        let refreshed =
            cache.summary_rows_for_workspaces([repo.path()], now + Duration::from_secs(61), false);
        assert_eq!(refreshed.rows[0].insertions, 2);
    }

    #[test]
    fn cache_skips_duplicate_workspace_paths_in_a_single_refresh() {
        let repo = TempDir::new("git-cache-duplicate-paths");
        init_repo_with_head(repo.path());

        let mut cache = GitSummaryCache::with_refresh_interval(Duration::from_secs(60));
        let report =
            cache.summary_rows_for_workspaces([repo.path(), repo.path()], Instant::now(), false);

        assert!(report.warnings.is_empty());
        assert_eq!(report.rows.len(), 1);
        assert_eq!(cache.repo_roots.len(), 1);
        assert_eq!(cache.summaries.len(), 1);
    }

    #[test]
    fn non_git_workspaces_fail_cleanly() {
        let dir = TempDir::new("git-cache-non-git");
        let mut cache = GitSummaryCache::new();

        let rows = cache.summary_rows_for_workspaces([dir.path()], Instant::now(), false);

        assert!(rows.rows.is_empty());
        assert!(rows.warnings.is_empty());
    }

    #[test]
    fn repositories_without_head_use_the_fallback_diff_paths() {
        let repo = TempDir::new("git-cache-no-head");
        run_git(repo.path(), &["init"]);
        run_git(repo.path(), &["config", "user.email", "ilmari@example.test"]);
        run_git(repo.path(), &["config", "user.name", "Ilmari Tests"]);
        run_git(repo.path(), &["branch", "-M", "main"]);

        fs::write(repo.path().join("tracked.txt"), "alpha\n").expect("staged file should exist");
        run_git(repo.path(), &["add", "tracked.txt"]);

        let mut cache = GitSummaryCache::new();
        let rows = cache.summary_rows_for_workspaces([repo.path()], Instant::now(), true);

        assert!(rows.warnings.is_empty());
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0].branch_name, "main");
        assert_eq!(rows.rows[0].insertions, 1);
        assert_eq!(rows.rows[0].deletions, 0);
    }

    #[test]
    fn cache_tracks_branch_names_per_worktree() {
        let repo = TempDir::new("git-cache-worktrees");
        init_repo_with_head(repo.path());

        let worktree = TempDir::new("git-cache-feature-worktree");
        run_git(
            repo.path(),
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.path().to_str().expect("worktree path should be utf-8"),
            ],
        );

        let mut cache = GitSummaryCache::new();
        let rows =
            cache.summary_rows_for_workspaces([repo.path(), worktree.path()], Instant::now(), true);

        assert!(rows.warnings.is_empty());
        assert_eq!(rows.rows.len(), 2);

        let mut branch_names: Vec<String> =
            rows.rows.into_iter().map(|row| row.branch_name).collect();
        branch_names.sort();

        assert_eq!(branch_names, vec!["feature".to_string(), "main".to_string()]);
    }

    #[test]
    fn broken_workspace_does_not_hide_healthy_repo_rows() {
        let repo = TempDir::new("git-cache-partial-failure");
        init_repo_with_head(repo.path());
        let missing = repo.path().join("missing");
        let mut cache = GitSummaryCache::new();

        let report = cache.summary_rows_for_workspaces(
            [repo.path(), missing.as_path()],
            Instant::now(),
            true,
        );

        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].branch_name, "main");
        assert_eq!(report.warnings.len(), 1);
    }

    #[test]
    fn cache_prunes_stale_workspace_and_repo_entries() {
        let first = TempDir::new("git-cache-prune-first");
        init_repo_with_head(first.path());
        let second = TempDir::new("git-cache-prune-second");
        init_repo_with_head(second.path());
        let mut cache = GitSummaryCache::with_refresh_interval(Duration::from_secs(60));
        let now = Instant::now();

        let first_report = cache.summary_rows_for_workspaces([first.path()], now, false);
        let first_root = first_report.rows[0].workspace_path.clone();
        assert_eq!(cache.repo_roots.len(), 1);
        assert_eq!(cache.summaries.len(), 1);
        assert!(cache.repo_roots.contains_key(first.path()));
        assert!(cache.summaries.contains_key(&first_root));

        let second_report =
            cache.summary_rows_for_workspaces([second.path()], now + Duration::from_secs(1), false);
        let second_root = second_report.rows[0].workspace_path.clone();
        assert_eq!(cache.repo_roots.len(), 1);
        assert_eq!(cache.summaries.len(), 1);
        assert!(!cache.repo_roots.contains_key(first.path()));
        assert!(!cache.summaries.contains_key(&first_root));
        assert!(cache.repo_roots.contains_key(second.path()));
        assert!(cache.summaries.contains_key(&second_root));

        cache.summary_rows_for_workspaces(
            std::iter::empty::<&Path>(),
            now + Duration::from_secs(2),
            false,
        );
        assert!(cache.repo_roots.is_empty());
        assert!(cache.summaries.is_empty());
    }

    fn init_repo_with_head(path: &Path) {
        run_git(path, &["init"]);
        run_git(path, &["config", "user.email", "ilmari@example.test"]);
        run_git(path, &["config", "user.name", "Ilmari Tests"]);
        fs::write(path.join("tracked.txt"), "alpha\n").expect("tracked file should exist");
        run_git(path, &["add", "tracked.txt"]);
        run_git(path, &["commit", "-m", "init"]);
        run_git(path, &["branch", "-M", "main"]);
    }

    fn run_git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .expect("git command should run");

        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("current time should be after epoch")
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).expect("temp dir should exist");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
