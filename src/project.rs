use anyhow::{Context as _, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::perf::{PROCESS_THRESHOLD, SlowOperation};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedProject {
    pub root: PathBuf,
    pub name: String,
}

pub fn validate_project(path: &Path) -> Result<ValidatedProject> {
    validate_project_with_git(path, "git")
}

fn validate_project_with_git(path: &Path, git_binary: &str) -> Result<ValidatedProject> {
    let _timing = SlowOperation::new(
        "project.validate",
        PROCESS_THRESHOLD,
        format!("path={}", path.display()),
    );
    if !path.exists() {
        bail!("{} does not exist", path.display());
    }
    if !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }

    let selected = path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", path.display()))?;
    let output = Command::new(git_binary)
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&selected)
        .output()
        .context("failed to run `git rev-parse`; is Git installed?")?;
    if !output.status.success() {
        bail!("{} is not inside a Git repository", selected.display());
    }
    let root_text =
        String::from_utf8(output.stdout).context("Git returned a non-UTF-8 repository path")?;
    let root = PathBuf::from(root_text.trim())
        .canonicalize()
        .context("failed to canonicalize the Git repository root")?;
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("Project")
        .to_owned();
    Ok(ValidatedProject { root, name })
}

#[cfg(test)]
mod tests {
    use super::{validate_project, validate_project_with_git};
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn canonicalizes_nested_paths_to_the_git_root_and_classifies_failures() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let fixture = std::env::temp_dir().join(format!("rode-project-validation-{nonce}"));
        let repository = fixture.join("repository");
        let nested = repository.join("src/nested");
        let plain = fixture.join("plain");
        let file = fixture.join("file.txt");
        fs::create_dir_all(&nested).expect("create Git fixture");
        fs::create_dir_all(&plain).expect("create plain fixture");
        fs::write(&file, "not a directory").expect("create file fixture");
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repository)
                .status()
                .expect("run git init")
                .success()
        );

        let validated = validate_project(&nested).expect("validate nested path");
        assert_eq!(validated.root, repository.canonicalize().unwrap());
        assert_eq!(validated.name, "repository");
        assert!(
            validate_project(&plain)
                .unwrap_err()
                .to_string()
                .contains("not inside")
        );
        assert!(
            validate_project(&fixture.join("missing"))
                .unwrap_err()
                .to_string()
                .contains("does not exist")
        );
        assert!(
            validate_project(&file)
                .unwrap_err()
                .to_string()
                .contains("not a directory")
        );
        assert!(
            validate_project_with_git(&nested, "rode-definitely-missing-git")
                .unwrap_err()
                .to_string()
                .contains("failed to run")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let linked = fixture.join("linked-repository");
            symlink(&repository, &linked).expect("create repository symlink");
            assert_eq!(
                validate_project(&linked).expect("validate symlink").root,
                repository.canonicalize().unwrap()
            );
        }

        fs::remove_dir_all(fixture).expect("clean project fixture");
    }
}
