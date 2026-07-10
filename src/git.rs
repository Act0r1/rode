use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Default)]
pub struct RepoSnapshot {
    pub root: PathBuf,
    pub is_repository: bool,
    pub branch: String,
    pub changed_files: usize,
    pub diff_stat: String,
    pub diff: String,
}

impl RepoSnapshot {
    pub fn load(path: &Path) -> Self {
        let root = canonical_or_original(path);
        if !git_ok(&root, &["rev-parse", "--is-inside-work-tree"]) {
            return Self {
                root,
                ..Self::default()
            };
        }

        let branch = git_output(&root, &["branch", "--show-current"])
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "detached HEAD".to_owned());
        let status = git_output(&root, &["status", "--porcelain=v1"]).unwrap_or_default();
        let changed_files = status.lines().count();
        let mut diff_stat = git_output(&root, &["diff", "--stat", "HEAD"]).unwrap_or_default();
        let mut diff =
            git_output(&root, &["diff", "--no-ext-diff", "--no-color", "HEAD"]).unwrap_or_default();
        let untracked = status
            .lines()
            .filter_map(|line| line.strip_prefix("?? "))
            .collect::<Vec<_>>();
        if !untracked.is_empty() {
            if !diff_stat.is_empty() {
                diff_stat.push('\n');
            }
            diff_stat.push_str(&format!("{} untracked file(s)", untracked.len()));
            if !diff.is_empty() {
                diff.push_str("\n\n");
            }
            diff.push_str("Untracked files:\n");
            for path in untracked {
                diff.push_str("  + ");
                diff.push_str(path);
                diff.push('\n');
            }
        }
        if diff_stat.is_empty() {
            diff_stat = "Working tree is clean".to_owned();
        }
        if diff.is_empty() {
            diff = "No uncommitted diff".to_owned();
        }

        Self {
            root,
            is_repository: true,
            branch,
            changed_files,
            diff_stat,
            diff,
        }
    }
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git_output(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::RepoSnapshot;
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn a_non_repository_is_reported_without_panicking() {
        let snapshot = RepoSnapshot::load(std::env::temp_dir().as_path());
        assert!(!snapshot.is_repository || !snapshot.branch.is_empty());
    }
    #[test]
    fn untracked_files_are_visible_in_snapshot_diff() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rode-git-test-{nonce}"));
        fs::create_dir_all(&root).expect("create temp repository");
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .expect("run git init");
        assert!(status.success());
        fs::write(root.join("new.txt"), "hello\n").expect("write fixture");

        let snapshot = RepoSnapshot::load(&root);
        assert_eq!(snapshot.changed_files, 1);
        assert!(snapshot.diff_stat.contains("1 untracked file"));
        assert!(snapshot.diff.contains("+ new.txt"));

        fs::remove_dir_all(root).expect("clean temp repository");
    }
}
