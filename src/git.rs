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

#[cfg(test)]
mod tests {
    use super::{RepoSnapshot, create_thread_worktree_at, remove_thread_worktree, safe_slug};
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
}
