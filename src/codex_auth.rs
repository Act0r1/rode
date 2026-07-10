use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const INITIALIZE_REQUEST_ID: u64 = 1;
const ACCOUNT_READ_REQUEST_ID: u64 = 2;
const LOGIN_REQUEST_ID: u64 = 3;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexAccount {
    ChatGpt { email: Option<String>, plan: String },
    ApiKey,
    Other(String),
}

impl CodexAccount {
    pub fn summary(&self) -> String {
        match self {
            Self::ChatGpt { email, plan } => {
                email.clone().unwrap_or_else(|| format!("ChatGPT {plan}"))
            }
            Self::ApiKey => "OpenAI API key".to_owned(),
            Self::Other(kind) => kind.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexAccountStatus {
    pub account: Option<CodexAccount>,
    pub requires_openai_auth: bool,
}

pub struct PendingCodexLogin {
    session: AppServerSession,
    login_id: String,
    pub auth_url: String,
}

impl PendingCodexLogin {
    pub fn wait(mut self) -> Result<CodexAccountStatus> {
        loop {
            let message = self.session.read_message()?;
            if message.get("method").and_then(Value::as_str) != Some("account/login/completed") {
                continue;
            }

            let params = message
                .get("params")
                .context("login completion notification is missing params")?;
            let completed_login_id = params.get("loginId").and_then(Value::as_str);
            if completed_login_id != Some(self.login_id.as_str()) {
                continue;
            }

            if !params
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let detail = params
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("the browser login did not complete");
                bail!("Codex login failed: {detail}");
            }

            return self.session.read_account();
        }
    }
}

pub fn read_codex_account() -> Result<CodexAccountStatus> {
    AppServerSession::start()?.read_account()
}

pub fn begin_codex_login() -> Result<PendingCodexLogin> {
    let mut session = AppServerSession::start()?;
    let result = session.request(
        LOGIN_REQUEST_ID,
        "account/login/start",
        json!({
            "type": "chatgpt",
            "useHostedLoginSuccessPage": true,
            "appBrand": "codex"
        }),
    )?;
    let login_id = required_string(&result, "loginId")?;
    let auth_url = required_string(&result, "authUrl")?;
    if !auth_url.starts_with("https://") {
        bail!("Codex returned a non-HTTPS authentication URL");
    }

    Ok(PendingCodexLogin {
        session,
        login_id,
        auth_url,
    })
}

struct AppServerSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl AppServerSession {
    fn start() -> Result<Self> {
        let mut child = Command::new("codex")
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start `codex app-server`; is Codex installed and on PATH?")?;
        let stdin = child
            .stdin
            .take()
            .context("Codex app-server has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Codex app-server has no stdout")?;
        let mut session = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        };

        session.request(
            INITIALIZE_REQUEST_ID,
            "initialize",
            json!({
                "clientInfo": {
                    "name": "rode",
                    "title": "Rode",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )?;
        session.send(&json!({ "method": "initialized" }))?;
        Ok(session)
    }

    fn read_account(&mut self) -> Result<CodexAccountStatus> {
        let result = self.request(
            ACCOUNT_READ_REQUEST_ID,
            "account/read",
            json!({ "refreshToken": false }),
        )?;
        parse_account_status(&result)
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value> {
        self.send(&json!({ "id": id, "method": method, "params": params }))?;
        loop {
            let message = self.read_message()?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                bail!("Codex app-server rejected {method}: {error}");
            }
            return message
                .get("result")
                .cloned()
                .with_context(|| format!("Codex app-server returned no result for {method}"));
        }
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, message)
            .context("failed to write to Codex app-server")?;
        self.stdin
            .write_all(b"\n")
            .context("failed to delimit Codex app-server request")?;
        self.stdin
            .flush()
            .context("failed to flush Codex app-server request")
    }

    fn read_message(&mut self) -> Result<Value> {
        let mut line = String::new();
        let bytes = self
            .stdout
            .read_line(&mut line)
            .context("failed to read from Codex app-server")?;
        if bytes == 0 {
            let status = self.child.try_wait().ok().flatten();
            return Err(anyhow!("Codex app-server closed unexpectedly ({status:?})"));
        }
        serde_json::from_str(&line).context("Codex app-server returned invalid JSON")
    }
}

impl Drop for AppServerSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_account_status(result: &Value) -> Result<CodexAccountStatus> {
    let requires_openai_auth = result
        .get("requiresOpenaiAuth")
        .and_then(Value::as_bool)
        .context("account/read result is missing requiresOpenaiAuth")?;
    let account = match result.get("account") {
        None | Some(Value::Null) => None,
        Some(account) => {
            let kind = required_string(account, "type")?;
            Some(match kind.as_str() {
                "chatgpt" => CodexAccount::ChatGpt {
                    email: account
                        .get("email")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    plan: account
                        .get("planType")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned(),
                },
                "apiKey" => CodexAccount::ApiKey,
                other => CodexAccount::Other(other.to_owned()),
            })
        }
    };
    Ok(CodexAccountStatus {
        account,
        requires_openai_auth,
    })
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .with_context(|| format!("Codex app-server result is missing {field}"))
}

#[cfg(test)]
mod tests {
    use super::{CodexAccount, CodexAccountStatus, parse_account_status, read_codex_account};
    use serde_json::json;

    #[test]
    fn parses_chatgpt_account_without_exposing_tokens() {
        let status = parse_account_status(&json!({
            "account": {
                "type": "chatgpt",
                "email": "developer@example.com",
                "planType": "plus"
            },
            "requiresOpenaiAuth": true
        }))
        .unwrap();

        assert_eq!(
            status,
            CodexAccountStatus {
                account: Some(CodexAccount::ChatGpt {
                    email: Some("developer@example.com".to_owned()),
                    plan: "plus".to_owned(),
                }),
                requires_openai_auth: true,
            }
        );
    }

    #[test]
    fn parses_signed_out_state() {
        let status = parse_account_status(&json!({
            "account": null,
            "requiresOpenaiAuth": true
        }))
        .unwrap();

        assert_eq!(status.account, None);
        assert!(status.requires_openai_auth);
    }

    #[test]
    #[ignore = "requires an installed Codex CLI"]
    fn installed_codex_app_server_reports_account_state() {
        let status = read_codex_account().unwrap();
        assert!(status.account.is_some() || status.requires_openai_auth);
    }
}
