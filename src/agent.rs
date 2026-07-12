use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderKind {
    Codex,
}

impl ProviderKind {
    fn executable(self) -> &'static str {
        match self {
            Self::Codex => "codex",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderStatus {
    pub kind: ProviderKind,
    pub available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderModel {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub is_default: bool,
    pub supports_images: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAccess {
    ReadOnly,
    #[default]
    WorkspaceWrite,
    FullAccess,
}

impl RuntimeAccess {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read only",
            Self::WorkspaceWrite => "Workspace write",
            Self::FullAccess => "Full access",
        }
    }

    pub fn storage_name(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::FullAccess => "full_access",
        }
    }

    pub fn from_storage_name(value: &str) -> Self {
        match value {
            "read_only" => Self::ReadOnly,
            "full_access" => Self::FullAccess,
            _ => Self::WorkspaceWrite,
        }
    }

    pub fn requires_confirmation(self) -> bool {
        self == Self::FullAccess
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnAttachment {
    GitDiff { text: String },
}

impl TurnAttachment {
    pub fn label(&self) -> &'static str {
        match self {
            Self::GitDiff { .. } => "Current Git diff",
        }
    }

    pub fn as_text_context(&self) -> String {
        match self {
            Self::GitDiff { text } => format!(
                "The user attached the current Git diff as context:\n<git_diff>\n{text}\n</git_diff>"
            ),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnRequest {
    pub local_thread_id: String,
    pub provider_thread_id: Option<String>,
    pub cwd: PathBuf,
    pub prompt: String,
    pub model: String,
    pub access: RuntimeAccess,
    pub attachments: Vec<TurnAttachment>,
    pub full_access_confirmed: bool,
}

pub fn discover_providers() -> Vec<ProviderStatus> {
    [ProviderKind::Codex]
        .into_iter()
        .map(|kind| {
            let path = find_executable(kind.executable());
            ProviderStatus {
                kind,
                available: path.is_some(),
            }
        })
        .collect()
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::{ProviderKind, RuntimeAccess, TurnAttachment, TurnRequest, discover_providers};

    #[test]
    fn discovery_always_returns_supported_providers() {
        let providers = discover_providers();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].kind, ProviderKind::Codex);
    }

    #[test]
    fn turn_request_captures_effective_configuration_immutably() {
        let request = TurnRequest {
            local_thread_id: "thread-1".to_owned(),
            provider_thread_id: Some("provider-thread-1".to_owned()),
            cwd: "/tmp/rode-worktree".into(),
            prompt: "Review this".to_owned(),
            model: "gpt-5.4".to_owned(),
            access: RuntimeAccess::ReadOnly,
            attachments: vec![TurnAttachment::GitDiff {
                text: "+safe change".to_owned(),
            }],
            full_access_confirmed: false,
        };
        let serialized = serde_json::to_string(&request).expect("serialize turn request");
        let restored: TurnRequest = serde_json::from_str(&serialized).expect("restore request");
        assert_eq!(restored, request);
        assert!(RuntimeAccess::FullAccess.requires_confirmation());
        assert!(!RuntimeAccess::WorkspaceWrite.requires_confirmation());
    }
}
