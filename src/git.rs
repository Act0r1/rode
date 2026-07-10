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
        let diff_stat = git_output(&root, &["diff", "--stat", "HEAD"])
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Working tree is clean".to_owned());
        let diff = git_output(
            &root,
            &["diff", "--no-ext-diff", "--no-color", "HEAD"],
        )
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "No uncommitted diff".to_owned());

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

    #[test]
    fn a_non_repository_is_reported_without_panicking() {
        let snapshot = RepoSnapshot::load(std::env::temp_dir().as_path());
        assert!(!snapshot.is_repository || !snapshot.branch.is_empty());
    }
}

