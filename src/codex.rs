use anyhow::{Context as _, Result, anyhow};
use async_channel::{Receiver, Sender};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, BufWriter, Write as _};
use std::path::Path;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum RpcId {
    Number(i64),
    String(String),
}

impl RpcId {
    fn from_value(value: &Value) -> Option<Self> {
        if let Some(number) = value.as_i64() {
            return Some(Self::Number(number));
        }
        value.as_str().map(|value| Self::String(value.to_owned()))
    }

    fn as_value(&self) -> Value {
        match self {
            Self::Number(value) => json!(value),
            Self::String(value) => json!(value),
        }
    }

    fn key(&self) -> String {
        match self {
            Self::Number(value) => value.to_string(),
            Self::String(value) => value.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalKind {
    Command,
    FileChange,
}

#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub rpc_id: RpcId,
    pub kind: ApprovalKind,
    pub item_id: String,
    pub title: String,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub enum CodexEvent {
    SessionReady {
        thread_id: String,
        model: String,
    },
    TurnStarted {
        turn_id: String,
    },
    AgentMessageDelta {
        delta: String,
    },
    AgentMessageCompleted {
        text: String,
    },
    ReasoningDelta {
        delta: String,
    },
    CommandStarted {
        item_id: String,
        command: String,
        cwd: String,
    },
    CommandCompleted {
        item_id: String,
        command: String,
        exit_code: Option<i64>,
        output: String,
    },
    FileChangeStarted {
        item_id: String,
        summary: String,
    },
    ApprovalRequested(ApprovalRequest),
    TurnCompleted {
        status: String,
        error: Option<String>,
    },
    Error(String),
    Exited,
}

type PendingResponse = mpsc::SyncSender<std::result::Result<Value, String>>;

struct Inner {
    writer: mpsc::Sender<Value>,
    pending: Arc<Mutex<HashMap<String, PendingResponse>>>,
    next_id: AtomicU64,
    child: Mutex<Child>,
    thread_id: Mutex<String>,
    active_turn_id: Mutex<Option<String>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Clone)]
pub struct CodexSession {
    inner: Arc<Inner>,
}

impl CodexSession {
    pub fn start(
        cwd: &Path,
        resume_thread_id: Option<&str>,
    ) -> Result<(Self, Receiver<CodexEvent>)> {
        let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let mut child = Command::new("codex")
            .args(["app-server", "--stdio"])
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| "failed to start `codex app-server --stdio`")?;
        let stdin = child
            .stdin
            .take()
            .context("Codex app-server has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Codex app-server has no stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("Codex app-server has no stderr")?;

        let (writer, writer_rx) = mpsc::channel::<Value>();
        let (events_tx, events_rx) = async_channel::unbounded();
        let pending = Arc::new(Mutex::new(HashMap::<String, PendingResponse>::new()));

        spawn_writer(stdin, writer_rx, events_tx.clone());
        spawn_reader(
            stdout,
            writer.clone(),
            Arc::clone(&pending),
            events_tx.clone(),
        );
        spawn_stderr_reader(stderr, events_tx.clone());

        let session = Self {
            inner: Arc::new(Inner {
                writer,
                pending,
                next_id: AtomicU64::new(1),
                child: Mutex::new(child),
                thread_id: Mutex::new(String::new()),
                active_turn_id: Mutex::new(None),
            }),
        };

        session.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "rode",
                    "title": "Rode",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": { "experimentalApi": true }
            }),
        )?;
        session.notify("initialized", None)?;

        let thread_params = json!({
            "cwd": cwd,
            "approvalPolicy": "on-request",
            "sandbox": "workspace-write"
        });
        let opened = match resume_thread_id {
            Some(thread_id) => {
                let mut resume = thread_params.clone();
                resume["threadId"] = json!(thread_id);
                session
                    .request("thread/resume", resume)
                    .or_else(|_| session.request("thread/start", thread_params.clone()))?
            }
            None => session.request("thread/start", thread_params)?,
        };
        let thread_id = opened
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .context("thread/start response did not include thread.id")?
            .to_owned();
        let model = opened
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("Codex")
            .to_owned();
        *session
            .inner
            .thread_id
            .lock()
            .map_err(|_| anyhow!("Codex thread lock is poisoned"))? = thread_id.clone();
        events_tx
            .send_blocking(CodexEvent::SessionReady { thread_id, model })
            .ok();

        Ok((session, events_rx))
    }

    pub fn start_turn(&self, prompt: &str) -> Result<String> {
        let thread_id = self.thread_id()?;
        let response = self.request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{ "type": "text", "text": prompt }],
                "approvalPolicy": "on-request",
                "sandboxPolicy": { "type": "workspaceWrite" }
            }),
        )?;
        let turn_id = response
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .context("turn/start response did not include turn.id")?
            .to_owned();
        *self
            .inner
            .active_turn_id
            .lock()
            .map_err(|_| anyhow!("Codex turn lock is poisoned"))? = Some(turn_id.clone());
        Ok(turn_id)
    }

    pub fn interrupt(&self) -> Result<()> {
        let turn_id = self
            .inner
            .active_turn_id
            .lock()
            .map_err(|_| anyhow!("Codex turn lock is poisoned"))?
            .clone();
        let Some(turn_id) = turn_id else {
            return Ok(());
        };
        self.request(
            "turn/interrupt",
            json!({ "threadId": self.thread_id()?, "turnId": turn_id }),
        )?;
        Ok(())
    }

    pub fn respond_to_approval(&self, request_id: &RpcId, decision: &str) -> Result<()> {
        self.send(json!({
            "id": request_id.as_value(),
            "result": { "decision": decision }
        }))
    }

    pub fn thread_id(&self) -> Result<String> {
        let thread_id = self
            .inner
            .thread_id
            .lock()
            .map_err(|_| anyhow!("Codex thread lock is poisoned"))?
            .clone();
        if thread_id.is_empty() {
            Err(anyhow!("Codex session has not opened a thread"))
        } else {
            Ok(thread_id)
        }
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let id_key = id.to_string();
        let (response_tx, response_rx) = mpsc::sync_channel(1);
        self.inner
            .pending
            .lock()
            .map_err(|_| anyhow!("Codex pending-request lock is poisoned"))?
            .insert(id_key.clone(), response_tx);
        if let Err(error) = self.send(json!({ "id": id, "method": method, "params": params })) {
            if let Ok(mut pending) = self.inner.pending.lock() {
                pending.remove(&id_key);
            }
            return Err(error);
        }
        match response_rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(anyhow!("Codex {method} failed: {error}")),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(mut pending) = self.inner.pending.lock() {
                    pending.remove(&id_key);
                }
                Err(anyhow!("Codex {method} timed out"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!(
                "Codex app-server exited while waiting for {method}"
            )),
        }
    }

    fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let mut message = json!({ "method": method });
        if let Some(params) = params {
            message["params"] = params;
        }
        self.send(message)
    }

    fn send(&self, message: Value) -> Result<()> {
        self.inner
            .writer
            .send(message)
            .map_err(|_| anyhow!("Codex app-server writer has stopped"))
    }
}

fn spawn_writer(
    stdin: std::process::ChildStdin,
    receiver: mpsc::Receiver<Value>,
    events: Sender<CodexEvent>,
) {
    thread::Builder::new()
        .name("rode-codex-writer".to_owned())
        .spawn(move || {
            let mut writer = BufWriter::new(stdin);
            for message in receiver {
                let write_result = serde_json::to_writer(&mut writer, &message)
                    .and_then(|_| writer.write_all(b"\n").map_err(serde_json::Error::io))
                    .and_then(|_| writer.flush().map_err(serde_json::Error::io));
                if let Err(error) = write_result {
                    events
                        .send_blocking(CodexEvent::Error(format!(
                            "Failed to write to Codex app-server: {error}"
                        )))
                        .ok();
                    break;
                }
            }
        })
        .expect("spawn Codex writer thread");
}

fn spawn_reader(
    stdout: std::process::ChildStdout,
    writer: mpsc::Sender<Value>,
    pending: Arc<Mutex<HashMap<String, PendingResponse>>>,
    events: Sender<CodexEvent>,
) {
    thread::Builder::new()
        .name("rode-codex-reader".to_owned())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(error) => {
                        events
                            .send_blocking(CodexEvent::Error(format!(
                                "Failed to read Codex app-server output: {error}"
                            )))
                            .ok();
                        break;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                let message: Value = match serde_json::from_str(&line) {
                    Ok(message) => message,
                    Err(error) => {
                        events
                            .send_blocking(CodexEvent::Error(format!(
                                "Invalid Codex app-server message: {error}"
                            )))
                            .ok();
                        continue;
                    }
                };
                route_incoming_message(message, &writer, &pending, &events);
            }

            if let Ok(mut pending) = pending.lock() {
                for (_, response) in pending.drain() {
                    response
                        .send(Err("Codex app-server output stream ended".to_owned()))
                        .ok();
                }
            }
            events.send_blocking(CodexEvent::Exited).ok();
            events.close();
        })
        .expect("spawn Codex reader thread");
}

fn spawn_stderr_reader(stderr: ChildStderr, events: Sender<CodexEvent>) {
    thread::Builder::new()
        .name("rode-codex-stderr".to_owned())
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let clean = strip_ansi(&line);
                if clean.contains(" ERROR ") || clean.starts_with("ERROR") {
                    events
                        .send_blocking(CodexEvent::Error(clean.trim().to_owned()))
                        .ok();
                }
            }
        })
        .expect("spawn Codex stderr thread");
}

fn route_incoming_message(
    message: Value,
    writer: &mpsc::Sender<Value>,
    pending: &Arc<Mutex<HashMap<String, PendingResponse>>>,
    events: &Sender<CodexEvent>,
) {
    let id = message.get("id").and_then(RpcId::from_value);
    let method = message.get("method").and_then(Value::as_str);

    match (id, method) {
        (Some(id), None) => {
            let response = if let Some(error) = message.get("error") {
                Err(error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown app-server error")
                    .to_owned())
            } else {
                Ok(message.get("result").cloned().unwrap_or(Value::Null))
            };
            if let Ok(mut pending) = pending.lock()
                && let Some(sender) = pending.remove(&id.key())
            {
                sender.send(response).ok();
            }
        }
        (Some(id), Some(method)) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            if let Some(event) = decode_server_request(id.clone(), method, &params) {
                events.send_blocking(event).ok();
            } else {
                writer
                    .send(json!({
                        "id": id.as_value(),
                        "error": {
                            "code": -32601,
                            "message": format!("Rode does not yet handle server request {method}")
                        }
                    }))
                    .ok();
                events
                    .send_blocking(CodexEvent::Error(format!(
                        "Unsupported Codex server request: {method}"
                    )))
                    .ok();
            }
        }
        (None, Some(method)) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            if let Some(event) = decode_notification(method, &params) {
                events.send_blocking(event).ok();
            }
        }
        (None, None) => {
            events
                .send_blocking(CodexEvent::Error(
                    "Unrecognized Codex app-server envelope".to_owned(),
                ))
                .ok();
        }
    }
}

fn decode_server_request(id: RpcId, method: &str, params: &Value) -> Option<CodexEvent> {
    let (kind, title, detail) = match method {
        "item/commandExecution/requestApproval" => {
            let command = params
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("Command execution");
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or("");
            (
                ApprovalKind::Command,
                command.to_owned(),
                [reason, cwd]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · "),
            )
        }
        "item/fileChange/requestApproval" => (
            ApprovalKind::FileChange,
            "Apply file changes".to_owned(),
            params
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("Codex requested permission to modify files")
                .to_owned(),
        ),
        _ => return None,
    };
    Some(CodexEvent::ApprovalRequested(ApprovalRequest {
        rpc_id: id,
        kind,
        item_id: params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        title,
        detail,
    }))
}

fn decode_notification(method: &str, params: &Value) -> Option<CodexEvent> {
    match method {
        "turn/started" => Some(CodexEvent::TurnStarted {
            turn_id: params
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        }),
        "item/agentMessage/delta" => Some(CodexEvent::AgentMessageDelta {
            delta: params.get("delta")?.as_str()?.to_owned(),
        }),
        "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
            Some(CodexEvent::ReasoningDelta {
                delta: params.get("delta")?.as_str()?.to_owned(),
            })
        }
        "item/started" => decode_item_started(params.get("item")?),
        "item/completed" => decode_item_completed(params.get("item")?),
        "turn/completed" => {
            let turn = params.get("turn")?;
            Some(CodexEvent::TurnCompleted {
                status: turn
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed")
                    .to_owned(),
                error: turn
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })
        }
        "error" => Some(CodexEvent::Error(
            params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Codex reported an error")
                .to_owned(),
        )),
        _ => None,
    }
}

fn decode_item_started(item: &Value) -> Option<CodexEvent> {
    match item.get("type")?.as_str()? {
        "commandExecution" => Some(CodexEvent::CommandStarted {
            item_id: item.get("id")?.as_str()?.to_owned(),
            command: item.get("command")?.as_str()?.to_owned(),
            cwd: item
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        }),
        "fileChange" => Some(CodexEvent::FileChangeStarted {
            item_id: item.get("id")?.as_str()?.to_owned(),
            summary: summarize_changes(item.get("changes")),
        }),
        _ => None,
    }
}

fn decode_item_completed(item: &Value) -> Option<CodexEvent> {
    match item.get("type")?.as_str()? {
        "agentMessage" => Some(CodexEvent::AgentMessageCompleted {
            text: item.get("text")?.as_str()?.to_owned(),
        }),
        "commandExecution" => Some(CodexEvent::CommandCompleted {
            item_id: item.get("id")?.as_str()?.to_owned(),
            command: item.get("command")?.as_str()?.to_owned(),
            exit_code: item.get("exitCode").and_then(Value::as_i64),
            output: item
                .get("aggregatedOutput")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        }),
        _ => None,
    }
}

fn summarize_changes(changes: Option<&Value>) -> String {
    let Some(changes) = changes.and_then(Value::as_array) else {
        return "Preparing file changes".to_owned();
    };
    let paths = changes
        .iter()
        .filter_map(|change| change.get("path").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if paths.is_empty() {
        format!("Preparing changes to {} file(s)", changes.len())
    } else {
        paths.join(", ")
    }
}

fn strip_ansi(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' && characters.peek() == Some(&'[') {
            characters.next();
            for character in characters.by_ref() {
                if character.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            result.push(character);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalKind, CodexEvent, CodexSession, RpcId, decode_notification, decode_server_request,
    };
    use serde_json::json;

    #[test]
    fn decodes_agent_message_delta() {
        let event = decode_notification(
            "item/agentMessage/delta",
            &json!({ "delta": "hello", "itemId": "item-1" }),
        );
        assert!(matches!(
            event,
            Some(CodexEvent::AgentMessageDelta { delta }) if delta == "hello"
        ));
    }

    #[test]
    fn decodes_command_approval_request() {
        let event = decode_server_request(
            RpcId::Number(7),
            "item/commandExecution/requestApproval",
            &json!({
                "itemId": "item-7",
                "command": "cargo publish",
                "cwd": "/workspace",
                "reason": "Requires network"
            }),
        );
        let Some(CodexEvent::ApprovalRequested(request)) = event else {
            panic!("expected approval request");
        };
        assert_eq!(request.kind, ApprovalKind::Command);
        assert_eq!(request.title, "cargo publish");
        assert!(request.detail.contains("Requires network"));
    }

    #[test]
    fn decodes_completed_command() {
        let event = decode_notification(
            "item/completed",
            &json!({
                "item": {
                    "type": "commandExecution",
                    "id": "command-1",
                    "command": "cargo test",
                    "exitCode": 0,
                    "aggregatedOutput": "4 passed"
                }
            }),
        );
        assert!(matches!(
            event,
            Some(CodexEvent::CommandCompleted { exit_code: Some(0), output, .. })
                if output == "4 passed"
        ));
    }

    #[test]
    #[ignore = "requires an installed and authenticated Codex CLI"]
    fn installed_codex_app_server_initializes_and_opens_a_thread() {
        let cwd = std::env::current_dir().expect("current directory");
        let (session, events) = CodexSession::start(&cwd, None).expect("start app-server session");
        assert!(!session.thread_id().expect("provider thread id").is_empty());
        assert!(matches!(
            events.recv_blocking(),
            Ok(CodexEvent::SessionReady { .. })
        ));
    }
}
