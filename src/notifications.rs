use std::process::{Command, Stdio};

pub fn turn_finished(thread_title: &str, status: &str, failed: bool) {
    let summary = if failed {
        "Rode · Codex turn needs attention"
    } else {
        "Rode · Codex turn complete"
    };
    let body = format!("{thread_title} · {status}");
    let _ = Command::new("notify-send")
        .args([
            "--app-name=Rode",
            if failed {
                "--urgency=critical"
            } else {
                "--urgency=normal"
            },
            summary,
            &body,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
