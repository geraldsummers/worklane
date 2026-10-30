use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::{Command, Stdio},
};
use uuid::Uuid;
use worklane_core::{ErrorReport, Store};

pub const API_VERSION: u32 = 1;
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_HERDR_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiRequest {
    pub version: u32,
    pub request_id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiResponse {
    pub version: u32,
    pub request_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiJob {
    pub operation_id: String,
    pub request_id: String,
    pub method: String,
    pub params: Value,
    pub state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorReport>,
}

fn error(code: &str, message: impl Into<String>, retryable: bool) -> ErrorReport {
    ErrorReport {
        code: code.into(),
        message: message.into(),
        guidance: None,
        lane_id: None,
        lane_name: None,
        retryable,
    }
}

fn response(request_id: String, result: Result<Value>) -> ApiResponse {
    match result {
        Ok(result) => ApiResponse {
            version: API_VERSION,
            request_id,
            ok: true,
            result: Some(result),
            error: None,
        },
        Err(problem) => ApiResponse {
            version: API_VERSION,
            request_id,
            ok: false,
            result: None,
            error: Some(error("api-request-failed", format!("{problem:#}"), false)),
        },
    }
}

pub fn schema() -> Value {
    json!({
        "version": API_VERSION,
        "transport": "one JSON request on stdin; one JSON response on stdout",
        "request": {"required": ["version", "request_id", "method"], "params_default": {}},
        "read_methods": {
            "lane.list": {},
            "lane.inspect": {"lane": "string", "fast": "boolean?"},
            "lane.refresh": {"lane": "string?", "all": "boolean?"},
            "lane.diff": {"lane": "string", "raw": "boolean?"},
            "lane.reconcile.report": {"lane": "string?", "all": "boolean?"},
            "operation.list": {},
            "operation.get": {"operation_id": "uuid"},
            "herdr.schema": {"lane": "string"},
            "tuios.schema": {"lane": "string", "note": "native daemon list-verbs; requires TUIOS in lane"},
            "tuios.read": {"lane": "string", "verb": "allowlisted native read verb", "params": "object"}
        },
        "change_methods": {
            "lane.create": {"name": "string", "project": "absolute provider-host path", "profile": "string?"},
            "lane.import": {"path": "absolute provider-host path"},
            "lane.open": {"lane": "string"},
            "lane.start": {"lane": "string"},
            "lane.stop": {"lane": "string"},
            "lane.rename": {"lane": "string", "new_name": "string"},
            "lane.reconcile.apply": {"lane": "string?", "all": "boolean?"},
            "lane.upgrade": {"lane": "string?", "all": "boolean?", "force": "boolean?", "no_cache": "boolean?"},
            "lane.delete": {"lane": "string"},
            "lane.forget": {"lane": "string"}
        },
        "control_methods": {
            "operation.cancel": {"operation_id": "uuid"},
            "herdr.call": {"lane": "string", "method": "native Herdr method", "params": "object"}
        },
        "job_states": ["queued", "running", "succeeded", "failed", "cancel_requested", "cancelled"],
        "limits": {"request_bytes": MAX_REQUEST_BYTES, "herdr_response_bytes": MAX_HERDR_RESPONSE_BYTES, "herdr_timeout_seconds": 55, "tuios_response_bytes": MAX_HERDR_RESPONSE_BYTES, "tuios_timeout_seconds": 55}
    })
}

fn state_dir() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| PathBuf::from(".local/state"))
        .join("worklane/api-jobs")
}

fn ensure_state_dir() -> Result<PathBuf> {
    let path = state_dir();
    fs::create_dir_all(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

fn validate_operation_id(id: &str) -> Result<()> {
    let parsed = Uuid::parse_str(id).context("operation_id must be a UUID")?;
    if parsed.to_string() != id {
        bail!("operation_id must use canonical lowercase UUID form")
    }
    Ok(())
}

fn job_path(id: &str) -> Result<PathBuf> {
    validate_operation_id(id)?;
    Ok(ensure_state_dir()?.join(format!("{id}.json")))
}

fn read_job(id: &str) -> Result<ApiJob> {
    let path = job_path(id)?;
    serde_json::from_slice(&fs::read(&path).with_context(|| format!("no operation '{id}'"))?)
        .with_context(|| format!("parse operation record {}", path.display()))
}

fn write_job(job: &ApiJob) -> Result<()> {
    let path = job_path(&job.operation_id)?;
    let temporary = path.with_extension(format!("json.{}.new", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer(&mut file, job)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, &path)?;
    Ok(())
}

fn list_jobs() -> Result<Vec<ApiJob>> {
    let mut jobs = Vec::new();
    for entry in fs::read_dir(ensure_state_dir()?)? {
        let path = entry?.path();
        if path.extension().and_then(|part| part.to_str()) != Some("json") {
            continue;
        }
        jobs.push(serde_json::from_slice(&fs::read(&path)?)?);
    }
    jobs.sort_by_key(|job: &ApiJob| std::cmp::Reverse(job.created_at));
    Ok(jobs)
}

fn required_string(params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .with_context(|| format!("params.{key} must be a non-empty string"))
}

fn optional_bool(params: &Value, key: &str) -> Result<bool> {
    match params.get(key) {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .with_context(|| format!("params.{key} must be a boolean")),
    }
}

fn optional_string(params: &Value, key: &str) -> Result<Option<String>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|value| !value.is_empty())
            .map(|value| Some(value.to_owned()))
            .with_context(|| format!("params.{key} must be a non-empty string")),
    }
}

fn lane_selector_args(params: &Value, base: &[&str]) -> Result<Vec<String>> {
    let mut args = base
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    if optional_bool(params, "all")? {
        args.push("--all".into());
    } else if let Some(lane) = optional_string(params, "lane")? {
        args.push(lane);
    } else {
        bail!("provide params.lane or set params.all=true")
    }
    Ok(args)
}

fn command_args(method: &str, params: &Value) -> Result<Vec<String>> {
    if !params.is_object() {
        bail!("params must be an object")
    }
    let one_lane = |action: &str| -> Result<Vec<String>> {
        Ok(vec![
            "lane".into(),
            action.into(),
            required_string(params, "lane")?,
        ])
    };
    match method {
        "lane.list" => Ok(vec!["lane".into(), "list".into()]),
        "lane.inspect" => {
            let mut args = one_lane("inspect")?;
            if optional_bool(params, "fast")? {
                args.push("--fast".into());
            }
            Ok(args)
        }
        "lane.refresh" => lane_selector_args(params, &["lane", "refresh"]),
        "lane.diff" => {
            let mut args = one_lane("diff")?;
            if optional_bool(params, "raw")? {
                args.push("--raw".into());
            }
            Ok(args)
        }
        "lane.reconcile.report" | "lane.reconcile.apply" => {
            let mut args = lane_selector_args(params, &["lane", "reconcile"])?;
            if method.ends_with("apply") {
                args.push("--apply".into());
            }
            Ok(args)
        }
        "lane.create" => {
            let project = PathBuf::from(required_string(params, "project")?);
            if !project.is_absolute() {
                bail!("params.project must be an absolute provider-host path")
            }
            let mut args = vec![
                "lane".into(),
                "create".into(),
                required_string(params, "name")?,
                "--project".into(),
                project.display().to_string(),
            ];
            if let Some(profile) = optional_string(params, "profile")? {
                args.extend(["--profile".into(), profile]);
            }
            Ok(args)
        }
        "lane.import" => {
            let path = PathBuf::from(required_string(params, "path")?);
            if !path.is_absolute() {
                bail!("params.path must be an absolute provider-host path")
            }
            Ok(vec![
                "lane".into(),
                "import".into(),
                path.display().to_string(),
            ])
        }
        "lane.open" => one_lane("open"),
        "lane.start" => one_lane("start"),
        "lane.stop" => one_lane("stop"),
        "lane.delete" => one_lane("delete"),
        "lane.forget" => one_lane("forget"),
        "lane.rename" => Ok(vec![
            "lane".into(),
            "rename".into(),
            required_string(params, "lane")?,
            required_string(params, "new_name")?,
        ]),
        "lane.upgrade" => {
            let mut args = lane_selector_args(params, &["lane", "upgrade"])?;
            if optional_bool(params, "force")? {
                args.push("--force".into());
            }
            if optional_bool(params, "no_cache")? {
                args.push("--no-cache".into());
            }
            Ok(args)
        }
        _ => bail!("unsupported lifecycle API method '{method}'"),
    }
}

fn invoke_worklane(method: &str, params: &Value) -> Result<(Value, Vec<Value>)> {
    let args = command_args(method, params)?;
    let output = Command::new(std::env::current_exe()?)
        .arg("--json")
        .args(args)
        .env("WORKLANE_EVENT_STREAM", "1")
        .output()
        .with_context(|| format!("run lifecycle method '{method}'"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let events = stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| json!({"message": line})))
        .collect::<Vec<_>>();
    if !output.status.success() {
        let message = events
            .last()
            .and_then(|event| event.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| stderr.trim());
        bail!("{message}")
    }
    let value = serde_json::from_slice(&output.stdout)
        .context("lifecycle command returned invalid JSON")?;
    Ok((value, events))
}

fn is_change_method(method: &str) -> bool {
    matches!(
        method,
        "lane.create"
            | "lane.import"
            | "lane.open"
            | "lane.start"
            | "lane.stop"
            | "lane.rename"
            | "lane.reconcile.apply"
            | "lane.upgrade"
            | "lane.delete"
            | "lane.forget"
    )
}

fn start_unit(id: &str) -> Result<()> {
    let executable = std::env::current_exe()?;
    let unit = format!("worklane-api-{id}");
    let mut command = Command::new("systemd-run");
    command.args([
        "--user",
        "--collect",
        "--quiet",
        "--unit",
        &unit,
        "--property=Type=exec",
        "--property=RuntimeMaxSec=6h",
        "--property=TasksMax=4096",
    ]);
    for name in [
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "CODEX_HOME",
        "GH_CONFIG_DIR",
        "TZ",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.arg(format!("--setenv={name}={}", value.to_string_lossy()));
        }
    }
    let output = command
        .arg(executable)
        .args(["api", "worker", id])
        .output()
        .context("start durable API operation with the user manager")?;
    if !output.status.success() {
        bail!(
            "systemd user manager could not start operation: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    Ok(())
}

fn submit(request: &ApiRequest) -> Result<Value> {
    command_args(&request.method, &request.params)?;
    let now = Utc::now();
    let mut job = ApiJob {
        operation_id: Uuid::new_v4().to_string(),
        request_id: request.request_id.clone(),
        method: request.method.clone(),
        params: request.params.clone(),
        state: "queued".into(),
        created_at: now,
        updated_at: now,
        events: Vec::new(),
        result: None,
        error: None,
    };
    write_job(&job)?;
    if std::env::var_os("WORKLANE_API_INLINE").is_some() {
        worker(&job.operation_id)?;
        job = read_job(&job.operation_id)?;
    } else if let Err(problem) = start_unit(&job.operation_id) {
        job.state = "failed".into();
        job.updated_at = Utc::now();
        job.error = Some(error("job-start-failed", format!("{problem:#}"), true));
        write_job(&job)?;
    }
    serde_json::to_value(job).map_err(Into::into)
}

fn unit_active(id: &str) -> bool {
    Command::new("systemctl")
        .args([
            "--user",
            "is-active",
            "--quiet",
            &format!("worklane-api-{id}.service"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn recover_jobs() -> Result<()> {
    if std::env::var_os("WORKLANE_API_INLINE").is_some() {
        return Ok(());
    }
    for mut job in list_jobs()? {
        if job.state == "cancel_requested" && !unit_active(&job.operation_id) {
            job.state = "cancelled".into();
            job.updated_at = Utc::now();
            write_job(&job)?;
        } else if matches!(job.state.as_str(), "queued" | "running")
            && !unit_active(&job.operation_id)
        {
            let _ = start_unit(&job.operation_id);
        }
    }
    Ok(())
}

fn cancel_job(id: &str) -> Result<Value> {
    let mut job = read_job(id)?;
    if matches!(job.state.as_str(), "succeeded" | "failed" | "cancelled") {
        return serde_json::to_value(job).map_err(Into::into);
    }
    job.state = "cancel_requested".into();
    job.updated_at = Utc::now();
    write_job(&job)?;
    let _ = Command::new("systemctl")
        .args(["--user", "stop", &format!("worklane-api-{id}.service")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    job.state = "cancelled".into();
    job.updated_at = Utc::now();
    write_job(&job)?;
    serde_json::to_value(job).map_err(Into::into)
}

pub fn worker(id: &str) -> Result<()> {
    validate_operation_id(id)?;
    let lock_path = ensure_state_dir()?.join(format!("{id}.lock"));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(());
    }
    let mut job = read_job(id)?;
    if matches!(job.state.as_str(), "succeeded" | "failed" | "cancelled") {
        return Ok(());
    }
    if job.state == "cancel_requested" {
        job.state = "cancelled".into();
        job.updated_at = Utc::now();
        return write_job(&job);
    }
    job.state = "running".into();
    job.updated_at = Utc::now();
    write_job(&job)?;
    match invoke_worklane(&job.method, &job.params) {
        Ok((result, events)) => {
            job = read_job(id)?;
            if matches!(job.state.as_str(), "cancel_requested" | "cancelled") {
                job.state = "cancelled".into();
            } else {
                job.state = "succeeded".into();
                job.result = Some(result);
            }
            job.events = events;
        }
        Err(problem) => {
            job = read_job(id)?;
            if matches!(job.state.as_str(), "cancel_requested" | "cancelled") {
                job.state = "cancelled".into();
            } else {
                job.state = "failed".into();
                job.error = Some(error(
                    "lifecycle-command-failed",
                    format!("{problem:#}"),
                    false,
                ));
            }
        }
    }
    job.updated_at = Utc::now();
    write_job(&job)
}

fn podman_json(
    spec: &worklane_core::LaneSpec,
    command: &[String],
    input: Option<&[u8]>,
) -> Result<Value> {
    if spec.host != "local" {
        bail!("Herdr API is available only for local lanes in API version {API_VERSION}")
    }
    let mut child = Command::new("timeout");
    child.args([
        "--signal=KILL",
        "60s",
        "podman",
        "exec",
        "--user",
        "dev",
        "--workdir",
        "/home/dev",
    ]);
    if input.is_some() {
        child.arg("--interactive");
    }
    child.arg(spec.container_name());
    child.args(command);
    child.stdout(Stdio::piped()).stderr(Stdio::piped());
    if input.is_some() {
        child.stdin(Stdio::piped());
    }
    let mut child = child
        .spawn()
        .context("enter lane container for Herdr API")?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .context("Herdr bridge stdin")?
            .write_all(input)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "lane Herdr API failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    if output.stdout.len() > MAX_HERDR_RESPONSE_BYTES {
        bail!("Herdr response exceeded {MAX_HERDR_RESPONSE_BYTES} bytes")
    }
    serde_json::from_slice(&output.stdout).context("lane Herdr API returned invalid JSON")
}

fn herdr_schema(params: &Value) -> Result<Value> {
    let store = Store::open_default()?;
    let spec = store.lane(&required_string(params, "lane")?)?;
    podman_json(
        &spec,
        &[
            "herdr".into(),
            "api".into(),
            "schema".into(),
            "--json".into(),
        ],
        None,
    )
}

fn schema_has_method(schema: &Value, method: &str) -> bool {
    schema["schemas"]["request"]["oneOf"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|entry| entry["properties"]["method"]["const"].as_str() == Some(method))
}

fn validate_herdr_call(method: &str, params: &Value, schema: &Value) -> Result<()> {
    if !params.is_object() {
        bail!("params.params must be an object")
    }
    if !schema_has_method(schema, method) {
        bail!("installed Herdr does not expose method '{method}'")
    }
    if matches!(
        method,
        "events.subscribe"
            | "server.live_handoff"
            | "pane.graphics.set"
            | "pane.graphics.clear"
            | "pane.graphics.info"
            | "pane.current"
    ) {
        bail!("Herdr method '{method}' is not available through the bounded connector bridge")
    }
    let required_id = if (method.starts_with("workspace.")
        && method != "workspace.create"
        && method != "workspace.list")
        || method == "tab.create"
    {
        Some("workspace_id")
    } else if method.starts_with("tab.") && method != "tab.list" {
        Some("tab_id")
    } else if method.starts_with("pane.") && !matches!(method, "pane.list" | "pane.split") {
        Some("pane_id")
    } else if method.starts_with("agent.") && !matches!(method, "agent.list" | "agent.start") {
        Some("target")
    } else {
        None
    };
    if let Some(key) = required_id {
        if params.get(key).and_then(Value::as_str).is_none() {
            bail!("Herdr method '{method}' requires explicit params.{key}; focused-pane defaults are not accepted")
        }
    }
    if method == "pane.split"
        && params
            .get("target_pane_id")
            .and_then(Value::as_str)
            .is_none()
        && params.get("workspace_id").and_then(Value::as_str).is_none()
    {
        bail!("Herdr method 'pane.split' requires explicit params.target_pane_id or params.workspace_id")
    }
    if method == "agent.start" && params.get("pane_id").and_then(Value::as_str).is_none() {
        bail!("Herdr method 'agent.start' requires explicit params.pane_id")
    }
    Ok(())
}

fn herdr_call(params: &Value) -> Result<Value> {
    let store = Store::open_default()?;
    let spec = store.lane(&required_string(params, "lane")?)?;
    let method = required_string(params, "method")?;
    let native_params = params.get("params").cloned().unwrap_or_else(|| json!({}));
    let schema = podman_json(
        &spec,
        &[
            "herdr".into(),
            "api".into(),
            "schema".into(),
            "--json".into(),
        ],
        None,
    )?;
    validate_herdr_call(&method, &native_params, &schema)?;
    let request = serde_json::to_vec(&json!({
        "id": format!("worklane:{}", Uuid::new_v4()),
        "method": method,
        "params": native_params
    }))?;
    let bridge = r#"import json, os, socket, sys
path = os.path.expanduser(sys.argv[1])
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(55)
s.connect(path)
payload = sys.stdin.buffer.read()
s.sendall(payload + b'\n')
buf = bytearray()
line = None
while line is None:
    while b'\n' in buf:
        candidate, _, remainder = buf.partition(b'\n')
        buf = bytearray(remainder)
        candidate = candidate.strip()
        if candidate:
            line = candidate
            break
    if line is not None:
        break
    chunk = s.recv(65536)
    if not chunk:
        candidate = bytes(buf).strip()
        if candidate:
            line = candidate
        break
    buf.extend(chunk)
    if len(buf) > 1048576:
        raise SystemExit('Herdr response exceeded 1048576 bytes')
if line is None:
    raise SystemExit('Herdr closed the socket without a JSON response')
json.loads(line)
sys.stdout.buffer.write(line)
"#;
    let socket = format!(
        "/home/dev/.config/herdr/sessions/{}/herdr.sock",
        spec.session_name()
    );
    podman_json(
        &spec,
        &["python3".into(), "-c".into(), bridge.into(), socket],
        Some(&request),
    )
}

// First v3 slice: native TUIOS discovery and bounded reads only. Do not silently
// route these to Herdr's compatibility socket: its IDs and method names differ.
fn validate_tuios_read(verb: &str, params: &Value, lane_session: &str) -> Result<()> {
    if !matches!(
        verb,
        "hello"
            | "list-sessions"
            | "list-workspaces"
            | "list-windows"
            | "session-info"
            | "get-window"
            | "list-agents"
            | "get-agent-state"
            | "capture-pane"
    ) {
        bail!("TUIOS verb '{verb}' is not in the read-only connector allowlist")
    }
    let object = params
        .as_object()
        .context("params.params must be an object")?;
    if object.contains_key("host") {
        bail!("TUIOS reads cannot target a remote host")
    }
    if verb != "hello"
        && verb != "list-sessions"
        && object.get("session").and_then(Value::as_str) != Some(lane_session)
    {
        bail!("TUIOS verb '{verb}' requires params.session matching the lane's session")
    }
    if matches!(verb, "capture-pane" | "get-window" | "get-agent-state")
        && object
            .get("window")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        bail!("TUIOS verb '{verb}' requires an explicit params.window")
    }
    Ok(())
}

fn tuios_socket_call(spec: &worklane_core::LaneSpec, verb: &str, params: &Value) -> Result<Value> {
    if spec.host != "local" {
        bail!("TUIOS API is available only for local lanes in API version {API_VERSION}")
    }
    let request = serde_json::to_vec(&json!({
        "id": format!("worklane:{}", Uuid::new_v4()),
        "verb": verb,
        "params": params,
    }))?;
    // TUIOS uses a line-delimited JSON socket distinct from its .herdr shim.
    // Resolve the lane user's runtime directory *inside* the lane, never on the host.
    let bridge = r#"import json, os, socket, sys
runtime = os.environ.get('XDG_RUNTIME_DIR') or '/run/user/' + str(os.getuid())
path = os.path.join(runtime, 'tuios', 'tuios.sock')
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(55)
s.connect(path)
s.sendall(sys.stdin.buffer.read() + b'\n')
line = bytearray()
while b'\n' not in line:
    chunk = s.recv(65536)
    if not chunk:
        raise SystemExit('TUIOS closed the socket without a complete response')
    line.extend(chunk)
    if len(line) > 1048576:
        raise SystemExit('TUIOS response exceeded 1048576 bytes')
result, remainder = bytes(line).split(b'\n', 1)
if remainder or not result:
    raise SystemExit('TUIOS returned an invalid response frame')
json.loads(result)
sys.stdout.buffer.write(result)
"#;
    let reply = podman_json(
        spec,
        &["python3".into(), "-c".into(), bridge.into()],
        Some(&request),
    )?;
    if reply.get("result").is_none() && reply.get("error").is_none() {
        bail!("TUIOS returned neither result nor error")
    }
    Ok(reply)
}

fn tuios_read(params: &Value, schema: bool) -> Result<Value> {
    let store = Store::open_default()?;
    let spec = store.lane(&required_string(params, "lane")?)?;
    let verb = if schema {
        "list-verbs".to_owned()
    } else {
        required_string(params, "verb")?
    };
    let native_params = if schema {
        json!({})
    } else {
        params.get("params").cloned().unwrap_or_else(|| json!({}))
    };
    if !schema {
        validate_tuios_read(&verb, &native_params, &spec.session_name())?;
    }
    tuios_socket_call(&spec, &verb, &native_params)
}

fn dispatch(request: &ApiRequest) -> Result<Value> {
    if request.version != API_VERSION {
        bail!(
            "API version mismatch: received {}, expected {API_VERSION}",
            request.version
        )
    }
    if request.request_id.is_empty() || request.request_id.len() > 128 {
        bail!("request_id must contain 1 to 128 characters")
    }
    recover_jobs()?;
    match request.method.as_str() {
        "schema" => Ok(schema()),
        "operation.list" => serde_json::to_value(list_jobs()?).map_err(Into::into),
        "operation.get" => serde_json::to_value(read_job(&required_string(
            &request.params,
            "operation_id",
        )?)?)
        .map_err(Into::into),
        "operation.cancel" => cancel_job(&required_string(&request.params, "operation_id")?),
        "herdr.schema" => herdr_schema(&request.params),
        "herdr.call" => herdr_call(&request.params),
        "tuios.schema" => tuios_read(&request.params, true),
        "tuios.read" => tuios_read(&request.params, false),
        method if is_change_method(method) => submit(request),
        method => invoke_worklane(method, &request.params).map(|(value, _)| value),
    }
}

pub fn run_stdio() -> Result<()> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut input)?;
    let api_response = if input.len() as u64 > MAX_REQUEST_BYTES {
        response(
            "unknown".into(),
            Err(anyhow::anyhow!(
                "API request exceeded {MAX_REQUEST_BYTES} bytes"
            )),
        )
    } else {
        match serde_json::from_slice::<ApiRequest>(&input) {
            Ok(request) => response(request.request_id.clone(), dispatch(&request)),
            Err(problem) => response("unknown".into(), Err(problem.into())),
        }
    };
    serde_json::to_writer(std::io::stdout(), &api_response)?;
    println!();
    Ok(())
}

pub fn print_schema() -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&schema())?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_separates_reads_changes_and_terminal_control() {
        let document = schema();
        assert!(document["read_methods"]["lane.list"].is_object());
        assert!(document["change_methods"]["lane.create"].is_object());
        assert!(document["control_methods"]["herdr.call"].is_object());
    }

    #[test]
    fn lifecycle_mapping_rejects_relative_provider_paths() {
        let problem = command_args("lane.create", &json!({"name":"x", "project":"relative"}))
            .unwrap_err()
            .to_string();
        assert!(problem.contains("absolute provider-host path"));
    }

    #[test]
    fn tuios_reads_require_explicit_lane_session_and_window() {
        let session = "lane-test";
        assert!(validate_tuios_read("list-sessions", &json!({}), session).is_ok());
        assert!(validate_tuios_read("list-windows", &json!({}), session).is_err());
        assert!(validate_tuios_read("list-windows", &json!({"session":"other"}), session).is_err());
        assert!(validate_tuios_read(
            "list-windows",
            &json!({"session":session,"host":"other"}),
            session
        )
        .is_err());
        assert!(validate_tuios_read("capture-pane", &json!({"session":session}), session).is_err());
        assert!(validate_tuios_read(
            "capture-pane",
            &json!({"session":session,"window":"abc"}),
            session
        )
        .is_ok());
        for verb in [
            "send-text",
            "new-session",
            "kill-session",
            "subscribe",
            "list-verbs",
        ] {
            assert!(validate_tuios_read(verb, &json!({"session":session}), session).is_err());
        }
        assert!(validate_tuios_read("hello", &json!([]), session).is_err());
    }

    #[test]
    fn bridge_rejects_focus_dependent_and_streaming_calls() {
        let schema = json!({"schemas":{"request":{"oneOf":[
            {"properties":{"method":{"const":"pane.current"}}},
            {"properties":{"method":{"const":"events.subscribe"}}},
            {"properties":{"method":{"const":"pane.read"}}}
        ]}}});
        assert!(validate_herdr_call("pane.current", &json!({}), &schema).is_err());
        assert!(validate_herdr_call("events.subscribe", &json!({}), &schema).is_err());
        assert!(validate_herdr_call("pane.read", &json!({}), &schema)
            .unwrap_err()
            .to_string()
            .contains("pane_id"));
    }
}
