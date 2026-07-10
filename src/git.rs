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
        let untracked = untracked_paths(&root);
        let mut diff_stat = git_output(&root, &["diff", "--stat", "HEAD"]).unwrap_or_default();
        let mut diff =
            git_output(&root, &["diff", "--no-ext-diff", "--no-color", "HEAD"]).unwrap_or_default();

        if !untracked.is_empty() {
            if !diff_stat.is_empty() {
                diff_stat.push('\n');
            }
            diff_stat.push_str(&format!("{} untracked file(s)", untracked.len()));
            for path in &untracked {
                if !diff.is_empty() {
                    diff.push_str("\n\n");
                }
                let bytes = std::fs::read(root.join(path)).unwrap_or_default();
                diff.push_str(&untracked_file_diff(path, &bytes));
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

fn untracked_paths(cwd: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .current_dir(cwd)
        .output()
        .ok();
    let Some(output) = output.filter(|output| output.status.success()) else {
        return Vec::new();
    };
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

fn untracked_file_diff(path: &str, bytes: &[u8]) -> String {
    let mut diff = format!(
        "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n"
    );
    let Ok(text) = std::str::from_utf8(bytes) else {
        diff.push_str(&format!("Binary files /dev/null and b/{path} differ"));
        return diff;
    };
    let line_count = text.lines().count();
    if bytes.contains(&0) || bytes.len() > 256 * 1024 || line_count > 5_000 {
        diff.push_str(&format!("Binary files /dev/null and b/{path} differ"));
        return diff;
    }
    if line_count == 0 {
        return diff;
    }

    diff.push_str(&format!("@@ -0,0 +1,{line_count} @@\n"));
    for line in text.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    if !text.ends_with('\n') {
        diff.push_str("\\ No newline at end of file\n");
    }
    diff
}

#[cfg(test)]
mod tests {
    use super::{RepoSnapshot, untracked_file_diff};
    use crate::diff::DiffDocument;

    #[test]
    fn a_non_repository_is_reported_without_panicking() {
        let snapshot = RepoSnapshot::load(std::env::temp_dir().as_path());
        assert!(!snapshot.is_repository || !snapshot.branch.is_empty());
    }

    #[test]
    fn untracked_text_files_are_rendered_as_added_hunks() {
        let diff = untracked_file_diff("notes/new file.txt", b"first\nsecond\n");
        let document = DiffDocument::parse(&diff);
        assert_eq!(document.files.len(), 1);
        assert_eq!(document.files[0].display_path(), "notes/new file.txt");
        assert_eq!(document.files[0].status.as_deref(), Some("Added"));
        assert_eq!(document.files[0].additions, 2);
    }

    #[test]
    fn untracked_binary_files_are_not_decoded_as_text() {
        let diff = untracked_file_diff("image.png", b"\x89PNG\0data");
        let document = DiffDocument::parse(&diff);
        assert!(document.files[0].binary);
        assert_eq!(document.files[0].status.as_deref(), Some("Added"));
    }
}
