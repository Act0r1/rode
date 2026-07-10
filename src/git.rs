use anyhow::{Context as _, Result, bail};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash as _, Hasher as _};
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadWorktree {
    pub path: PathBuf,
    pub branch: String,
}

pub fn create_thread_worktree(
    repository: &Path,
    thread_id: &str,
    title: &str,
) -> Result<ThreadWorktree> {
    create_thread_worktree_at(
        repository,
        thread_id,
        title,
        &rode_state_dir()?.join("worktrees"),
    )
}

fn create_thread_worktree_at(
    repository: &Path,
    thread_id: &str,
    title: &str,
    worktrees_root: &Path,
) -> Result<ThreadWorktree> {
    let repository = canonical_or_original(repository);
    let git_root = git_output(&repository, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .context("the selected project is not inside a Git repository")?;
    if !git_ok(&git_root, &["rev-parse", "--verify", "HEAD"]) {
        bail!("the repository needs an initial commit before Rode can create worktrees");
    }

    let short_id = safe_slug(thread_id).chars().take(12).collect::<String>();
    let title_slug = safe_slug(title);
    let branch = if title_slug.is_empty() {
        format!("rode/{short_id}")
    } else {
        format!("rode/{short_id}-{title_slug}")
    };
    let repository_key = repository_key(&git_root);
    let path = worktrees_root.join(repository_key).join(&short_id);
    if path.exists() {
        bail!("thread worktree already exists at {}", path.display());
    }
    let parent = path.parent().context("worktree path has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create worktree directory {}", parent.display()))?;

    let output = Command::new("git")
        .args(["worktree", "add", "-b"])
        .arg(&branch)
        .arg(&path)
        .arg("HEAD")
        .current_dir(&git_root)
        .output()
        .context("failed to start `git worktree add`")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if path.exists() {
            let _ = fs::remove_dir_all(&path);
        }
        bail!("git worktree add failed: {detail}");
    }

    Ok(ThreadWorktree { path, branch })
}

#[allow(dead_code)] // Used by the upcoming persisted thread-deletion UI and by lifecycle tests.
pub fn remove_thread_worktree(repository: &Path, worktree: &ThreadWorktree) -> Result<()> {
    let git_root = git_output(repository, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .context("the selected project is not inside a Git repository")?;
    let output = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&worktree.path)
        .current_dir(&git_root)
        .output()
        .context("failed to start `git worktree remove`")?;
    if !output.status.success() {
        bail!(
            "git worktree remove failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

pub fn commit_all(repository: &Path, message: &str) -> Result<String> {
    let message = message.trim();
    if message.is_empty() {
        bail!("enter a commit message first");
    }
    let root = repository_root(repository)?;
    let status = git_output(&root, &["status", "--porcelain=v1"]).unwrap_or_default();
    if status.is_empty() {
        bail!("the working tree has no changes to commit");
    }
    git_command(&root, &["add", "--all"])?;
    git_command(&root, &["commit", "-m", message])?;
    git_output(&root, &["rev-parse", "--short", "HEAD"])
        .context("Git did not return the new commit ID")
}

pub fn push_current_branch(repository: &Path) -> Result<String> {
    let root = repository_root(repository)?;
    let branch = git_output(&root, &["branch", "--show-current"])
        .filter(|branch| !branch.is_empty())
        .context("cannot push a detached HEAD")?;
    git_command(&root, &["push", "--set-upstream", "origin", &branch])?;
    Ok(branch)
}

pub fn create_pull_request(repository: &Path, title: &str) -> Result<String> {
    let title = title.trim();
    if title.is_empty() {
        bail!("enter a pull-request title first");
    }
    let root = repository_root(repository)?;
    let branch = git_output(&root, &["branch", "--show-current"])
        .filter(|branch| !branch.is_empty())
        .context("cannot create a pull request from a detached HEAD")?;
    let output = Command::new("gh")
        .args([
            "pr",
            "create",
            "--title",
            title,
            "--body",
            "Created with Rode.",
            "--head",
            &branch,
        ])
        .current_dir(&root)
        .output()
        .context("failed to start `gh`; install GitHub CLI and authenticate it first")?;
    if !output.status.success() {
        bail!(
            "gh pr create failed: {}",
            command_detail(&output.stdout, &output.stderr)
        );
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if url.is_empty() {
        bail!("gh created the pull request but returned no URL");
    }
    Ok(url)
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

fn repository_root(path: &Path) -> Result<PathBuf> {
    let path = canonical_or_original(path);
    git_output(&path, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .context("the selected workspace is not inside a Git repository")
}

fn git_command(cwd: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to start `git {}`", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().copied().unwrap_or("command"),
            command_detail(&output.stdout, &output.stderr)
        );
    }
    Ok(())
}

fn command_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    if stdout.is_empty() {
        "command exited unsuccessfully".to_owned()
    } else {
        stdout
    }
}

fn rode_state_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("rode"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/rode"))
}

fn repository_key(path: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .map(safe_slug)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "repository".to_owned());
    format!("{name}-{:016x}", hasher.finish())
}

fn safe_slug(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    let mut previous_dash = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            previous_dash = false;
        } else if !previous_dash && !slug.is_empty() {
            slug.push('-');
            previous_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
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
    use super::{
        RepoSnapshot, commit_all, create_thread_worktree_at, git_output, remove_thread_worktree,
        safe_slug, untracked_file_diff,
    };
    use crate::diff::DiffDocument;
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
        assert!(snapshot.diff.contains("+++ b/new.txt"));
        assert!(snapshot.diff.contains("+hello"));

        fs::remove_dir_all(root).expect("clean temp repository");
    }

    #[test]
    fn thread_worktree_is_created_on_an_isolated_branch() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let fixture = std::env::temp_dir().join(format!("rode-worktree-test-{nonce}"));
        let repository = fixture.join("repository");
        let state = fixture.join("state");
        fs::create_dir_all(&repository).expect("create repository fixture");
        for args in [
            vec!["init", "--quiet"],
            vec!["config", "user.name", "Rode Test"],
            vec!["config", "user.email", "rode@example.invalid"],
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&repository)
                    .status()
                    .expect("run git fixture command")
                    .success()
            );
        }
        fs::write(repository.join("README.md"), "fixture\n").expect("write fixture file");
        assert!(
            Command::new("git")
                .args(["add", "README.md"])
                .current_dir(&repository)
                .status()
                .expect("git add fixture")
                .success()
        );
        assert!(
            Command::new("git")
                .args(["commit", "--quiet", "-m", "initial"])
                .current_dir(&repository)
                .status()
                .expect("git commit fixture")
                .success()
        );

        let worktree = create_thread_worktree_at(
            &repository,
            "thread-1234567890",
            "Add Native Terminal",
            &state,
        )
        .expect("create thread worktree");
        assert!(worktree.path.join("README.md").is_file());
        assert_eq!(worktree.branch, "rode/thread-12345-add-native-terminal");
        let branch = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&worktree.path)
            .output()
            .expect("read worktree branch");
        assert_eq!(
            String::from_utf8_lossy(&branch.stdout).trim(),
            worktree.branch
        );

        remove_thread_worktree(&repository, &worktree).expect("remove thread worktree");
        fs::remove_dir_all(fixture).expect("clean worktree fixture");
    }

    #[test]
    fn slug_is_safe_for_branches_and_paths() {
        assert_eq!(safe_slug("  Fix: Wayland / HiDPI  "), "fix-wayland-hidpi");
        assert_eq!(safe_slug("../../"), "");
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

    #[test]
    fn commit_all_stages_and_commits_the_reviewed_worktree() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rode-commit-test-{nonce}"));
        fs::create_dir_all(&root).expect("create repository fixture");
        for args in [
            ["init", "--quiet"].as_slice(),
            ["config", "user.name", "Rode Test"].as_slice(),
            ["config", "user.email", "rode@example.invalid"].as_slice(),
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&root)
                    .status()
                    .expect("run git fixture command")
                    .success()
            );
        }
        fs::write(root.join("README.md"), "reviewed\n").expect("write reviewed change");

        let commit = commit_all(&root, "feat: commit from Rode").expect("commit changes");
        assert!(!commit.is_empty());
        assert_eq!(
            git_output(&root, &["status", "--porcelain=v1"]).as_deref(),
            Some("")
        );
        assert_eq!(
            git_output(&root, &["log", "-1", "--pretty=%s"]).as_deref(),
            Some("feat: commit from Rode")
        );

        fs::remove_dir_all(root).expect("clean repository fixture");
    }
}
