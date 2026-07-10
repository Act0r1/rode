use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderKind {
    Codex,
    Claude,
}

impl ProviderKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
        }
    }

    fn executable(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderStatus {
    pub kind: ProviderKind,
    pub available: bool,
    pub path: Option<PathBuf>,
}

pub fn discover_providers() -> Vec<ProviderStatus> {
    [ProviderKind::Codex, ProviderKind::Claude]
        .into_iter()
        .map(|kind| {
            let path = find_executable(kind.executable());
            ProviderStatus {
                kind,
                available: path.is_some(),
                path,
            }
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct AgentRun {
    pub thread_id: Option<String>,
    pub message: String,
}

pub fn run_codex(cwd: &Path, prompt: &str, thread_id: Option<&str>) -> Result<AgentRun> {
    let mut command = Command::new("codex");
    command.current_dir(cwd);

    match thread_id {
        Some(thread_id) => {
            command.args(["exec", "resume", "--json", thread_id, prompt]);
        }
        None => {
            command.args([
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--sandbox",
                "workspace-write",
                "-c",
                "approval_policy=\"never\"",
                prompt,
            ]);
        }
    }

    let output = command
        .output()
        .with_context(|| "failed to start the Codex CLI; is it installed and on PATH?")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();

    let mut discovered_thread_id = thread_id.map(ToOwned::to_owned);
    let mut messages = Vec::new();
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) == Some("thread.started") {
            discovered_thread_id = event
                .get("thread_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        if let Some(item) = event.get("item")
            && item.get("type").and_then(Value::as_str) == Some("agent_message")
            && let Some(text) = item.get("text").and_then(Value::as_str)
        {
            messages.push(text.to_owned());
        }
    }

    if !output.status.success() {
        let detail = if stderr.is_empty() {
            stdout.trim().to_owned()
        } else {
            stderr
        };
        return Err(anyhow!("Codex exited with {}: {detail}", output.status));
    }

    let message = messages
        .pop()
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| "The turn completed without an agent message.".to_owned());
    Ok(AgentRun {
        thread_id: discovered_thread_id,
        message,
    })
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::{ProviderKind, discover_providers};

    #[test]
    fn discovery_always_returns_supported_providers() {
        let providers = discover_providers();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].kind, ProviderKind::Codex);
        assert_eq!(providers[1].kind, ProviderKind::Claude);
    }
}
