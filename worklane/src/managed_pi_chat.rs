//! A bounded, tool-free Pi RPC session INSIDE the already-owned managed Worklane container.
//! No host model call, ambient Pi credentials, shell command, or retry of uncertain prompts.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use worklane_core::LaneSpec;

const MAX_PROMPT: usize = 8192;
const MAX_REPLY: usize = 32768;
const MAX_RECORD: usize = 131072;
const MAX_STREAM: usize = 8 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(150);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChatRequest {
    pub generation: String,
    pub lane_id: String,
    pub session: String,
    pub owner_subject: String,
    pub intent_id: String,
    pub message: String,
}
#[derive(Serialize)]
pub(crate) struct ChatResult<'a> {
    generation: &'a str,
    lane_id: &'a str,
    session: &'a str,
    intent_id: &'a str,
    message: &'a str,
}
impl<'a> ChatResult<'a> {
    pub(crate) fn new(request: &'a ChatRequest, message: &'a str) -> Self {
        Self {
            generation: &request.generation,
            lane_id: &request.lane_id,
            session: &request.session,
            intent_id: &request.intent_id,
            message,
        }
    }
}
fn validate(spec: &LaneSpec, request: &ChatRequest) -> Result<()> {
    let parent = spec.project_path.parent().context("unbound managed lane")?;
    if parent.file_name().and_then(|v| v.to_str()) != Some("managed")
        || parent
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|v| v.to_str())
            != Some(request.generation.as_str())
        || request.generation.len() != 14
        || !request.generation.starts_with("q-")
        || !request.generation[2..]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || request.lane_id != spec.id
        || request.session != spec.session_name()
        || !uuid::Uuid::parse_str(&request.owner_subject)
            .is_ok_and(|v| v.to_string() == request.owner_subject)
        || request.intent_id.len() != 32
        || !request
            .intent_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || request.message.is_empty()
        || request.message.len() > MAX_PROMPT
        || request.message.contains('\0')
    {
        bail!("foreign or unbounded Pi RPC chat request")
    }
    Ok(())
}
fn receipt_directory(path: &Path) -> Result<()> {
    let info = fs::symlink_metadata(path)?;
    if !info.file_type().is_dir()
        || info.permissions().mode() & 0o077 != 0
        || info.uid() != unsafe { libc::geteuid() }
        || path.canonicalize()? != path
    {
        bail!("unsafe managed chat receipt directory")
    }
    Ok(())
}
fn write_new(path: &Path, value: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(value)?;
    file.sync_all()?;
    fs::File::open(path.parent().context("receipt without parent")?)?.sync_all()?;
    Ok(())
}
fn interpret(record: &Value, accepted: &mut bool, answer: &mut Option<String>) -> Result<bool> {
    match record.get("type").and_then(Value::as_str) {
        Some("response") if record.get("command").and_then(Value::as_str) == Some("prompt") => {
            if record.get("id").and_then(Value::as_str) != Some("device-prompt")
                || record.get("success").and_then(Value::as_bool) != Some(true)
                || record.pointer("/data/disposition").and_then(Value::as_str) != Some("started")
            {
                bail!("Pi prompt was not started")
            }
            *accepted = true;
        }
        Some("message_end")
            if *accepted
                && record.pointer("/message/role").and_then(Value::as_str) == Some("assistant") =>
        {
            let content = record
                .pointer("/message/content")
                .and_then(Value::as_array)
                .context("Pi assistant content missing")?;
            let mut text = String::new();
            for block in content {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    text.push_str(
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .context("invalid Pi text block")?,
                    );
                    if text.len() > MAX_REPLY {
                        bail!("Pi answer exceeded bound")
                    }
                }
            }
            if record
                .pointer("/message/stopReason")
                .and_then(Value::as_str)
                == Some("error")
            {
                bail!("Pi provider error")
            }
            if !text.is_empty() {
                *answer = Some(text)
            }
        }
        Some("agent_settled") => {
            if !*accepted
                || record.get("aborted").and_then(Value::as_bool) != Some(false)
                || answer.is_none()
            {
                bail!("Pi session settled without a completed assistant reply")
            }
            return Ok(true);
        }
        _ => {}
    }
    Ok(false)
}
/// Caller holds Worklane's exclusive native lane lock and has verified the
/// container owner, managed generation binding, broker process, and rootless store.
/// An intent survives every failed/ambiguous child or lost result: NEVER retry it.
pub(crate) fn chat(spec: &LaneSpec, request: &ChatRequest, root: &Path) -> Result<String> {
    validate(spec, request)?;
    receipt_directory(root)?;
    let dir = root.join("v3-pi-chat");
    if !dir.exists() {
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        fs::File::open(root)?.sync_all()?
    }
    receipt_directory(&dir)?;
    if fs::read_dir(&dir)?.any(|entry| {
        entry.is_ok_and(|entry| {
            let path = entry.path();
            path.extension().is_some_and(|v| v == "intent")
                && !path.with_extension("result").exists()
        })
    }) {
        bail!("uncertain prior Pi chat intent; independent reconciliation required")
    }
    let intent = dir.join(format!("{}.intent", request.intent_id));
    // No prompt or token is stored in the host receipt; the session lives only in this fresh lane.
    write_new(&intent, serde_json::to_string(&json!({"generation":request.generation,
        "lane_id":request.lane_id,"session":request.session,"owner_subject":request.owner_subject,"intent_id":request.intent_id}))?.as_bytes())?;
    let mut child = Command::new("podman")
        .args([
            "exec",
            "-i",
            "--user",
            "dev",
            "--env",
            "PI_CODING_AGENT_DIR=/run/worklane-pi",
            &spec.container_name(),
            "/usr/local/bin/pi",
            "--mode",
            "rpc",
            "--no-tools",
            "--no-extensions",
            "--no-mcp",
            "--no-skills",
            "--no-context-files",
            "--no-prompt-templates",
            "--provider",
            "openai",
            "--model",
            "gpt-6-sol",
            "--session-dir",
            "/home/dev/.pi/agent/sessions",
            "--session-id",
        ])
        .arg(format!(
            "device-{}-{}-{}",
            request.generation, request.lane_id, request.session
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let outcome = (|| -> Result<String> {
        let mut stdin = child.stdin.take().context("Pi RPC stdin absent")?;
        stdin.write_all(
            format!(
                "{}\n",
                json!({"id":"device-prompt","type":"prompt","message":request.message})
            )
            .as_bytes(),
        )?;
        stdin.flush()?;
        // Keep stdin open until agent_settled: EOF before that event can abort a run.
        let stdout = child.stdout.take().context("Pi RPC stdout absent")?;
        let (tx, rx) = mpsc::sync_channel::<Result<Vec<u8>>>(4);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut total = 0usize;
            loop {
                let mut row = Vec::new();
                let read = reader
                    .by_ref()
                    .take((MAX_RECORD + 1) as u64)
                    .read_until(b'\n', &mut row);
                match read {
                    Ok(0) => break,
                    Ok(n) => {
                        total += n;
                        if n > MAX_RECORD || total > MAX_STREAM || !row.ends_with(b"\n") {
                            let _ = tx.send(Err(anyhow::anyhow!("unbounded Pi RPC output")));
                            break;
                        }
                        if tx.send(Ok(row)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error.into()));
                        break;
                    }
                }
            }
        });
        let start = Instant::now();
        let mut accepted = false;
        let mut answer = None;
        loop {
            let row = rx.recv_timeout(
                DEADLINE
                    .checked_sub(start.elapsed())
                    .context("Pi RPC deadline expired")?,
            )??;
            let record: Value = serde_json::from_slice(&row)?;
            if interpret(&record, &mut accepted, &mut answer)? {
                break;
            }
        }
        let text = answer.context("missing Pi answer")?;
        drop(stdin);
        let exit_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    bail!("Pi RPC did not exit cleanly")
                }
                break;
            }
            if Instant::now() >= exit_deadline {
                bail!("Pi RPC exit deadline expired")
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(text)
    })();
    if child.try_wait()?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    if let Ok(ref text) = outcome {
        let result = dir.join(format!("{}.result", request.intent_id));
        write_new(&result,serde_json::to_string(&json!({"sha256":format!("{:x}",sha2::Sha256::digest(text.as_bytes())),
            "generation":request.generation,"lane_id":request.lane_id,"session":request.session,"intent_id":request.intent_id}))?.as_bytes())?;
        // Keep the original intent and its result; future admission checks only unresolved intents.
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_settled_real_pi_assistant_completes_a_turn() {
        let mut accepted = false;
        let mut reply = None;
        assert!(!interpret(
            &json!({"id":"device-prompt","type":"response","command":"prompt",
            "success":true,"data":{"disposition":"started"}}),
            &mut accepted,
            &mut reply
        )
        .unwrap());
        assert!(accepted);
        assert!(!interpret(
            &json!({"type":"message_end","message":{"role":"assistant",
            "content":[{"type":"text","text":"real Pi reply"}],"stopReason":"stop"}}),
            &mut accepted,
            &mut reply
        )
        .unwrap());
        assert!(!interpret(&json!({"type":"agent_end"}), &mut accepted, &mut reply).unwrap());
        assert!(interpret(
            &json!({"type":"agent_settled","aborted":false}),
            &mut accepted,
            &mut reply
        )
        .unwrap());
        assert_eq!(reply.as_deref(), Some("real Pi reply"));
    }
    #[test]
    fn rejects_handled_prompts_abort_and_provider_failure() {
        let mut accepted = false;
        let mut reply = None;
        assert!(interpret(
            &json!({"id":"device-prompt","type":"response","command":"prompt",
            "success":true,"data":{"disposition":"handled"}}),
            &mut accepted,
            &mut reply
        )
        .is_err());
        accepted = true;
        assert!(interpret(
            &json!({"type":"agent_settled","aborted":true}),
            &mut accepted,
            &mut reply
        )
        .is_err());
        assert!(interpret(
            &json!({"type":"message_end","message":{"role":"assistant",
            "content":[{"type":"text","text":"failed"}],"stopReason":"error"}}),
            &mut accepted,
            &mut reply
        )
        .is_err());
    }
}
