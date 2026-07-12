use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use crate::perf::{RPC_THRESHOLD, SlowOperation};

const INITIALIZE_REQUEST_ID: u64 = 1;
const ACCOUNT_READ_REQUEST_ID: u64 = 2;
const LOGIN_REQUEST_ID: u64 = 3;
const LOGIN_CANCEL_REQUEST_ID: u64 = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexAccount {
    ChatGpt { email: Option<String>, plan: String },
    ApiKey,
    Other(String),
}

impl CodexAccount {
    pub fn summary(&self) -> String {
        match self {
            Self::ChatGpt { email, plan } => email
                .as_ref()
                .map(|email| format!("{email} · ChatGPT {plan}"))
                .unwrap_or_else(|| format!("ChatGPT {plan}")),
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
    cancel_requested: Arc<AtomicBool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexLoginOutcome {
    Complete(CodexAccountStatus),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoginCancelStatus {
    Canceled,
    NotFound,
}

#[derive(Clone)]
pub struct PendingCodexLoginCancellation {
    stdin: Arc<Mutex<ChildStdin>>,
    login_id: String,
    cancel_requested: Arc<AtomicBool>,
}

impl PendingCodexLoginCancellation {
    pub fn cancel(&self) -> Result<()> {
        send_message(&self.stdin, &login_cancel_message(&self.login_id))?;
        self.cancel_requested.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl PendingCodexLogin {
    pub fn cancellation(&self) -> PendingCodexLoginCancellation {
        PendingCodexLoginCancellation {
            stdin: self.session.stdin.clone(),
            login_id: self.login_id.clone(),
            cancel_requested: self.cancel_requested.clone(),
        }
    }

    pub fn wait(mut self) -> Result<CodexLoginOutcome> {
        loop {
            let message = self.session.read_message()?;
            if let Some(cancel) = parse_login_cancel_response(&message) {
                match cancel? {
                    LoginCancelStatus::Canceled => return Ok(CodexLoginOutcome::Cancelled),
                    LoginCancelStatus::NotFound => continue,
                }
            }
            if let Some(completion) = parse_login_completion(&message, &self.login_id) {
                if let Err(error) = completion {
                    if self.cancel_requested.load(Ordering::SeqCst) {
                        return Err(error).context(
                            "login failed before Codex confirmed the cancellation request",
                        );
                    }
                    return Err(error);
                }
                return self.session.read_account().map(CodexLoginOutcome::Complete);
            }
        }
    }
}

fn login_cancel_message(login_id: &str) -> Value {
    json!({
        "id": LOGIN_CANCEL_REQUEST_ID,
        "method": "account/login/cancel",
        "params": { "loginId": login_id }
    })
}

fn parse_login_cancel_response(message: &Value) -> Option<Result<LoginCancelStatus>> {
    if message.get("id").and_then(Value::as_u64) != Some(LOGIN_CANCEL_REQUEST_ID) {
        return None;
    }
    if let Some(error) = message.get("error") {
        return Some(Err(anyhow!(
            "Codex app-server rejected account/login/cancel: {error}"
        )));
    }
    let Some(status) = message.pointer("/result/status").and_then(Value::as_str) else {
        return Some(Err(anyhow!(
            "Codex app-server returned a malformed login cancellation response"
        )));
    };
    Some(match status {
        "canceled" => Ok(LoginCancelStatus::Canceled),
        "notFound" => Ok(LoginCancelStatus::NotFound),
        other => Err(anyhow!(
            "Codex app-server returned unknown login cancellation status {other:?}"
        )),
    })
}

fn parse_login_completion(message: &Value, expected_login_id: &str) -> Option<Result<()>> {
    if message.get("method").and_then(Value::as_str) != Some("account/login/completed") {
        return None;
    }

    let Some(params) = message.get("params") else {
        return Some(Err(anyhow!(
            "login completion notification is missing params"
        )));
    };
    if let Some(completed_login_id) = params.get("loginId").and_then(Value::as_str)
        && completed_login_id != expected_login_id
    {
        return None;
    }

    if params
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some(Ok(()));
    }

    let detail = params
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("the browser login did not complete");
    Some(Err(anyhow!("Codex login failed: {detail}")))
}

pub fn read_codex_account() -> Result<CodexAccountStatus> {
    AppServerSession::start()?.read_account()
}

pub fn begin_codex_login() -> Result<PendingCodexLogin> {
    let mut session = AppServerSession::start()?;
    let result = session.request(
        LOGIN_REQUEST_ID,
        "account/login/start",
        chatgpt_login_params(),
    )?;
    let (login_id, auth_url) = parse_login_start(&result)?;

    Ok(PendingCodexLogin {
        session,
        login_id,
        auth_url,
        cancel_requested: Arc::new(AtomicBool::new(false)),
    })
}

fn chatgpt_login_params() -> Value {
    json!({
        "type": "chatgpt",
        // The Codex-branded hosted page redirects to `codex://threads/new/`.
        // Rode receives completion over app-server, so keep success in the
        // browser instead of claiming another client's URI scheme.
        "useHostedLoginSuccessPage": false
    })
}

fn parse_login_start(result: &Value) -> Result<(String, String)> {
    let login_id = required_string(result, "loginId")?;
    let auth_url = required_string(result, "authUrl")?;
    if !auth_url.starts_with("https://") {
        bail!("Codex returned a non-HTTPS authentication URL");
    }
    Ok((login_id, auth_url))
}

struct AppServerSession {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
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
            stdin: Arc::new(Mutex::new(stdin)),
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
        let _timing =
            SlowOperation::new("codex.auth_rpc", RPC_THRESHOLD, format!("method={method}"));
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

    fn send(&self, message: &Value) -> Result<()> {
        send_message(&self.stdin, message)
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

fn send_message(stdin: &Arc<Mutex<ChildStdin>>, message: &Value) -> Result<()> {
    let mut stdin = stdin
        .lock()
        .map_err(|_| anyhow!("Codex app-server stdin lock was poisoned"))?;
    serde_json::to_writer(&mut *stdin, message).context("failed to write to Codex app-server")?;
    stdin
        .write_all(b"\n")
        .context("failed to delimit Codex app-server request")?;
    stdin
        .flush()
        .context("failed to flush Codex app-server request")
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
    use super::{
        CodexAccount, CodexAccountStatus, LoginCancelStatus, begin_codex_login,
        chatgpt_login_params, login_cancel_message, parse_account_status,
        parse_login_cancel_response, parse_login_completion, parse_login_start, read_codex_account,
    };
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
    fn starts_the_managed_codex_browser_flow_with_a_local_success_page() {
        assert_eq!(
            chatgpt_login_params(),
            json!({
                "type": "chatgpt",
                "useHostedLoginSuccessPage": false
            })
        );
        assert_eq!(
            parse_login_start(&json!({
                "loginId": "login-123",
                "authUrl": "https://chatgpt.com/auth"
            }))
            .unwrap(),
            (
                "login-123".to_owned(),
                "https://chatgpt.com/auth".to_owned()
            )
        );
    }

    #[test]
    fn refuses_to_open_an_insecure_login_url() {
        let error = parse_login_start(&json!({
            "loginId": "login-123",
            "authUrl": "http://example.com/auth"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("non-HTTPS"));
    }

    #[test]
    fn accepts_login_completion_with_a_nullable_login_id() {
        let completion = parse_login_completion(
            &json!({
                "method": "account/login/completed",
                "params": { "loginId": null, "success": true, "error": null }
            }),
            "login-123",
        )
        .expect("completion notification");
        completion.unwrap();
    }

    #[test]
    fn ignores_completion_for_a_different_non_null_login_id() {
        let completion = parse_login_completion(
            &json!({
                "method": "account/login/completed",
                "params": { "loginId": "other-login", "success": true, "error": null }
            }),
            "login-123",
        );
        assert!(completion.is_none());
    }

    #[test]
    fn cancels_the_exact_managed_login_id() {
        assert_eq!(
            login_cancel_message("login-123"),
            json!({
                "id": 4,
                "method": "account/login/cancel",
                "params": { "loginId": "login-123" }
            })
        );
        assert_eq!(
            parse_login_cancel_response(&json!({
                "id": 4,
                "result": { "status": "canceled" }
            }))
            .unwrap()
            .unwrap(),
            LoginCancelStatus::Canceled
        );
        assert_eq!(
            parse_login_cancel_response(&json!({
                "id": 4,
                "result": { "status": "notFound" }
            }))
            .unwrap()
            .unwrap(),
            LoginCancelStatus::NotFound
        );
        assert!(
            parse_login_cancel_response(&json!({
                "id": 4,
                "error": { "code": -32602, "message": "bad login" }
            }))
            .unwrap()
            .is_err()
        );
        assert!(
            parse_login_cancel_response(&json!({ "id": 4, "result": {} }))
                .unwrap()
                .is_err()
        );
    }

    #[test]
    #[ignore = "requires an installed Codex CLI"]
    fn installed_codex_app_server_reports_account_state() {
        let status = read_codex_account().unwrap();
        assert!(status.account.is_some() || status.requires_openai_auth);
    }

    #[test]
    #[ignore = "requires an installed Codex CLI"]
    fn installed_codex_app_server_starts_browser_login() {
        let login = begin_codex_login().unwrap();
        assert!(login.auth_url.starts_with("https://"));
    }
}
