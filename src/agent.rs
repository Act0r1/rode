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
    use super::{ProviderKind, discover_providers};

    #[test]
    fn discovery_always_returns_supported_providers() {
        let providers = discover_providers();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].kind, ProviderKind::Codex);
    }
}
