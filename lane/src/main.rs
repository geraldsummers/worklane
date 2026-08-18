use anyhow::{Context, Result};
use crossterm::{
    event::{self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use worklane_core::{LaneStatus, Store};

static FORCE_FULL_REDRAW: AtomicBool = AtomicBool::new(false);

struct App {
    lanes: Vec<LaneStatus>,
    selected: usize,
    filter: String,
    filtering: bool,
    detail: String,
    lane_errors: HashMap<String, String>,
    message: String,
    confirming_delete: bool,
    confirming_forget: bool,
    confirming_reconcile: bool,
    rename_input: Option<String>,
    jobs: Vec<ActiveJob>,
    job_queue: VecDeque<JobRequest>,
    state_poll: Option<Receiver<StatePollResult>>,
    state_poll_sender: Option<Sender<StatePollResult>>,
    state_poll_queue: VecDeque<StatePollRequest>,
    state_poll_inflight: usize,
    state_poll_pending: usize,
    state_poll_errors: Vec<String>,
    refresh_generations: HashMap<String, u64>,
    next_refresh_generation: u64,
}
enum JobEvent {
    Progress(String),
    Complete(JobResult),
}
struct JobResult {
    label: String,
    verb: String,
    output: Result<CommandOutput, String>,
    cached_lanes: Option<Vec<LaneStatus>>,
}
struct ActiveJob {
    lane_id: Option<String>,
    host: Option<String>,
    verb: String,
    label: String,
    started: Instant,
    receiver: Receiver<JobEvent>,
}
struct JobRequest {
    lane_id: Option<String>,
    host: Option<String>,
    label: String,
    verb: String,
    args: Vec<String>,
}
struct CommandOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
struct StatePollResult {
    lane_id: String,
    lane_name: String,
    generation: u64,
    output: Result<std::process::Output, String>,
}
struct StatePollRequest {
    lane_id: String,
    lane_name: String,
    generation: u64,
}
#[derive(Debug, PartialEq, Eq)]
enum UiAction {
    None,
    Quit,
    Command(&'static str, bool),
    Rename(String),
    UpgradeAll,
    Reload,
    EditContainerfile,
    Reconcile(bool),
}
impl App {
    fn load() -> Result<Self> {
        let mut lanes = load_registered_lanes()?;
        sort_lanes(&mut lanes);
        let mut app = Self {
            lanes,
            selected: 0,
            filter: String::new(),
            filtering: false,
            detail: String::new(),
            lane_errors: HashMap::new(),
            message: String::new(),
            confirming_delete: false,
            confirming_forget: false,
            confirming_reconcile: false,
            rename_input: None,
            jobs: Vec::new(),
            job_queue: VecDeque::new(),
            state_poll: None,
            state_poll_sender: None,
            state_poll_queue: VecDeque::new(),
            state_poll_inflight: 0,
            state_poll_pending: 0,
            state_poll_errors: Vec::new(),
            refresh_generations: HashMap::new(),
            next_refresh_generation: 0,
        };
        start_state_poll(&mut app);
        Ok(app)
    }
    fn visible(&self) -> Vec<&LaneStatus> {
        self.lanes
            .iter()
            .filter(|x| x.spec.name.contains(&self.filter) || x.spec.host.contains(&self.filter))
            .collect()
    }
    fn handle_key(&mut self, key: KeyCode) -> UiAction {
        if let Some(input) = &mut self.rename_input {
            return match key {
                KeyCode::Esc => {
                    self.rename_input = None;
                    self.message = "rename cancelled".into();
                    UiAction::None
                }
                KeyCode::Enter if !input.is_empty() => {
                    let name = input.clone();
                    self.rename_input = None;
                    UiAction::Rename(name)
                }
                KeyCode::Backspace => {
                    input.pop();
                    UiAction::None
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    UiAction::None
                }
                _ => UiAction::None,
            };
        }
        if self.filtering {
            return match key {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.filtering = false;
                    self.selected = 0;
                    UiAction::None
                }
                KeyCode::Enter => {
                    self.filtering = false;
                    UiAction::None
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.selected = 0;
                    UiAction::None
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.selected = 0;
                    UiAction::None
                }
                _ => UiAction::None,
            };
        }
        if self.confirming_delete {
            return match key {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.confirming_delete = false;
                    UiAction::Command("delete", false)
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirming_delete = false;
                    self.message = "delete cancelled".into();
                    UiAction::None
                }
                _ => UiAction::None,
            };
        }
        if self.confirming_forget {
            return match key {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.confirming_forget = false;
                    UiAction::Command("forget", false)
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirming_forget = false;
                    self.message = "forget cancelled".into();
                    UiAction::None
                }
                _ => UiAction::None,
            };
        }
        if self.confirming_reconcile {
            return match key {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.confirming_reconcile = false;
                    UiAction::Reconcile(true)
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirming_reconcile = false;
                    self.message = "reconcile apply cancelled".into();
                    UiAction::None
                }
                _ => UiAction::None,
            };
        }
        match key {
            KeyCode::Esc if !self.detail.is_empty() || !self.lane_errors.is_empty() => {
                if let Some(lane_id) = selected_lane_id(self) {
                    self.lane_errors.remove(&lane_id);
                }
                self.detail.clear();
                self.message = "details closed".into();
                UiAction::None
            }
            KeyCode::Char('q') => UiAction::Quit,
            KeyCode::Char('j') | KeyCode::Down => {
                let count = self.visible().len();
                if count > 0 {
                    self.selected = (self.selected + 1).min(count - 1);
                }
                UiAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                UiAction::None
            }
            KeyCode::Char('s') => UiAction::Command("start", false),
            KeyCode::Char('x') => UiAction::Command("stop", false),
            KeyCode::Char('u') => UiAction::Command("upgrade", false),
            KeyCode::Char('U') => UiAction::UpgradeAll,
            KeyCode::Char('a') => UiAction::Command("attach", false),
            KeyCode::Char('S') => UiAction::Command("attach", true),
            KeyCode::Char('d') => UiAction::Command("diff", false),
            KeyCode::Char('D') if self.visible().get(self.selected).is_some() => {
                self.confirming_delete = true;
                let lane = self.visible()[self.selected];
                self.message = format!(
                    "Delete '{}' ({}) on {}? Container and manifest are removed; project {} is preserved. y/N",
                    lane.spec.name,
                    lane.spec.id,
                    lane.spec.host,
                    lane.spec.project_path.display()
                );
                UiAction::None
            }
            KeyCode::Char('F') if self.visible().get(self.selected).is_some() => {
                self.confirming_forget = true;
                let lane = self.visible()[self.selected];
                self.message = format!(
                    "Forget '{}' ({}) on {} from this registry? Container, manifest, and project {} are preserved. y/N",
                    lane.spec.name,
                    lane.spec.id,
                    lane.spec.host,
                    lane.spec.project_path.display()
                );
                UiAction::None
            }
            KeyCode::Char('i') => UiAction::Command("inspect", false),
            KeyCode::Char('n') if self.visible().get(self.selected).is_some() => {
                self.rename_input = Some(String::new());
                self.message = "enter a new lane name; Enter saves, Esc cancels".into();
                UiAction::None
            }
            KeyCode::Char('r') => UiAction::Reload,
            KeyCode::Char('c') => UiAction::Reconcile(false),
            KeyCode::Char('C') if self.visible().get(self.selected).is_some() => {
                self.confirming_reconcile = true;
                let lane = self.visible()[self.selected];
                self.message = format!(
                    "Apply verified reconciliation for '{}' ({}) on {}? y/N",
                    lane.spec.name, lane.spec.id, lane.spec.host
                );
                UiAction::None
            }
            KeyCode::Char('e') => UiAction::EditContainerfile,
            KeyCode::Char('/') => {
                self.filter.clear();
                self.filtering = true;
                self.selected = 0;
                UiAction::None
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.selected = 0;
                UiAction::None
            }
            _ => UiAction::None,
        }
    }
}
fn load_registered_lanes() -> Result<Vec<LaneStatus>> {
    Store::open_default()?.lanes()
}
fn load_cached_lanes() -> Result<Vec<LaneStatus>> {
    Store::open_default()?.lanes()
}
fn sort_lanes(lanes: &mut [LaneStatus]) {
    lanes.sort_by(|a, b| {
        b.spec
            .last_attached
            .cmp(&a.spec.last_attached)
            .then_with(|| a.spec.name.cmp(&b.spec.name))
    });
}
fn clamp_selection(app: &mut App) {
    app.selected = app.selected.min(app.visible().len().saturating_sub(1));
}
fn selected_lane_id(app: &App) -> Option<String> {
    app.visible()
        .get(app.selected)
        .map(|lane| lane.spec.id.clone())
}
fn restore_selection(app: &mut App, selected_id: Option<&str>) {
    sort_lanes(&mut app.lanes);
    if let Some(selected_id) = selected_id {
        if let Some(index) = app
            .visible()
            .iter()
            .position(|lane| lane.spec.id == selected_id)
        {
            app.selected = index;
            return;
        }
    }
    clamp_selection(app);
}
fn replace_lanes(app: &mut App, lanes: Vec<LaneStatus>) {
    let selected_id = selected_lane_id(app);
    let mut lanes = lanes;
    for lane in &mut lanes {
        if lane.state == "running" && lane.runtime_started_at.is_none() {
            lane.runtime_started_at = app
                .lanes
                .iter()
                .find(|existing| existing.spec.id == lane.spec.id)
                .and_then(|existing| existing.runtime_started_at);
        }
    }
    app.lanes = lanes;
    restore_selection(app, selected_id.as_deref());
}
fn sync_registered_lanes(app: &mut App) -> Result<bool> {
    let lanes = load_registered_lanes()?;
    let old_ids = app
        .lanes
        .iter()
        .map(|lane| lane.spec.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let changed = lanes.len() != app.lanes.len()
        || lanes
            .iter()
            .any(|lane| !old_ids.contains(lane.spec.id.as_str()));
    replace_lanes(app, lanes);
    Ok(changed)
}
fn invalidate_lane_refresh(app: &mut App, lane_id: &str) {
    app.next_refresh_generation += 1;
    app.refresh_generations
        .insert(lane_id.into(), app.next_refresh_generation);
    app.state_poll_queue
        .retain(|request| request.lane_id != lane_id);
}
fn action(app: &mut App, verb: &str) -> Result<bool> {
    action_with_args(app, verb, &[])
}
fn upgrade_detail(output: &[u8]) -> Result<String> {
    let values: Vec<serde_json::Value> = serde_json::from_slice(output)?;
    if values.is_empty() {
        return Ok("No lanes were selected for upgrade.".into());
    }
    Ok(values
        .iter()
        .map(|value| {
            let lane = value["spec"]["name"]
                .as_str()
                .or_else(|| value["lane_name"].as_str())
                .unwrap_or("unknown lane");
            if let Some(outcome) = value["outcome"].as_str() {
                format!("{lane}: {outcome}")
            } else {
                format!(
                    "{lane}: {}{}",
                    value["state"].as_str().unwrap_or("upgraded"),
                    if value["drift"].as_bool().unwrap_or(false) {
                        " (drift)"
                    } else {
                        ""
                    }
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n"))
}
fn inspect_detail(output: &[u8]) -> Result<String> {
    let value: serde_json::Value = serde_json::from_slice(output)?;
    let spec = &value["spec"];
    Ok(format!(
        "Name: {}\nHost: {}\nState: {}{}\nProject: {}\nImage: {}\nHome: /home/dev",
        spec["name"].as_str().unwrap_or("unknown"),
        spec["host"].as_str().unwrap_or("unknown"),
        value["state"].as_str().unwrap_or("unknown"),
        if value["drift"].as_bool().unwrap_or(false) {
            " (drift)"
        } else {
            ""
        },
        spec["project_path"].as_str().unwrap_or("unknown"),
        spec["profile"]["image"].as_str().unwrap_or("unknown"),
    ))
}
fn command_error(stderr: &[u8], fallback: &str) -> String {
    let error = String::from_utf8_lossy(stderr).trim().to_owned();
    if error.is_empty() {
        fallback.to_owned()
    } else {
        error
    }
}
fn action_with_args(app: &mut App, verb: &str, extra: &[&str]) -> Result<bool> {
    let Some(lane_id) = app
        .visible()
        .get(app.selected)
        .map(|item| item.spec.id.clone())
    else {
        app.message = format!("{verb}: no lane selected");
        return Ok(false);
    };
    invalidate_lane_refresh(app, &lane_id);
    let mut command = Command::new("worklane");
    command.args(["lane", verb, &lane_id]).args(extra);
    if verb == "attach" {
        restore_terminal_state();
        let _resume = ResumeTui;
        let mut child = command
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to start worklane attach")?;
        let mut child_stderr = child
            .stderr
            .take()
            .context("worklane attach stderr unavailable")?;
        let stderr_thread = std::thread::spawn(move || -> io::Result<Vec<u8>> {
            let mut captured = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let count = child_stderr.read(&mut chunk)?;
                if count == 0 {
                    return Ok(captured);
                }
                captured.extend_from_slice(&chunk[..count]);
                io::stderr().write_all(&chunk[..count])?;
                io::stderr().flush()?;
            }
        });
        let status = child.wait().context("failed to wait for worklane attach")?;
        let stderr = stderr_thread
            .join()
            .map_err(|_| anyhow::anyhow!("worklane attach error reader panicked"))?
            .context("failed to read worklane attach error output")?;
        app.message = format!("{verb}: {}", if status.success() { "ok" } else { "failed" });
        if !status.success() {
            app.detail = command_error(
                &stderr,
                &format!("worklane attach exited with {status} without an error message"),
            );
            app.lane_errors.insert(lane_id, app.detail.clone());
        } else {
            app.lane_errors.remove(&lane_id);
        }
        return Ok(status.success());
    }
    let output = command.output()?;
    if output.status.success() {
        if verb == "diff" {
            let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            let lines = value["diff"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>();
            app.detail = if lines.is_empty() {
                "No meaningful writable-root changes.".into()
            } else {
                lines.join("\n")
            };
        } else if verb == "inspect" {
            app.detail = inspect_detail(&output.stdout)?;
        } else if verb == "upgrade" {
            app.detail = upgrade_detail(&output.stdout)?;
        } else {
            app.detail.clear();
        }
        app.message = format!("{verb}: ok");
        replace_lanes(app, load_cached_lanes()?);
    } else {
        app.message = format!("{verb}: failed");
        app.detail = command_error(
            &output.stderr,
            &format!("worklane {verb} failed without an error message"),
        );
    }
    Ok(output.status.success())
}
fn start_job(app: &mut App, verb: &str, extra: &[String], all: bool) -> Result<()> {
    let (args, lane_id, host) = if all {
        let mut args = vec!["lane".into(), verb.into(), "--all".into()];
        args.extend(extra.iter().cloned());
        (args, None, None)
    } else {
        let visible = app.visible();
        let Some(item) = visible.get(app.selected) else {
            return Ok(());
        };
        let mut args = vec!["lane".into(), verb.into(), item.spec.id.clone()];
        args.extend(extra.iter().cloned());
        (
            args,
            Some(item.spec.id.clone()),
            Some(item.spec.host.clone()),
        )
    };
    let label = if all {
        format!("{verb} all")
    } else {
        verb.to_string()
    };
    let conflicts = app.jobs.iter().any(|job| {
        lane_id.is_some() && job.lane_id == lane_id
            || verb == "upgrade" && job.verb == "upgrade" && job.host == host
    }) || app.job_queue.iter().any(|job| {
        lane_id.is_some() && job.lane_id == lane_id
            || verb == "upgrade" && job.verb == "upgrade" && job.host == host
    });
    if conflicts {
        app.message = format!("{label}: already queued or running");
        return Ok(());
    }
    if let Some(lane_id) = &lane_id {
        invalidate_lane_refresh(app, lane_id);
    }
    app.job_queue.push_back(JobRequest {
        lane_id,
        host,
        label: label.clone(),
        verb: verb.into(),
        args,
    });
    app.message = format!("{label}: queued");
    app.detail = format!("{label}: queued");
    start_queued_jobs(app);
    Ok(())
}
fn start_queued_jobs(app: &mut App) {
    const MAX_EXTERNAL_JOBS: usize = 4;
    while app.jobs.len() < MAX_EXTERNAL_JOBS {
        let eligible = app.job_queue.iter().position(|request| {
            let same_host = request.host.as_ref().map_or(0, |host| {
                app.jobs
                    .iter()
                    .filter(|job| job.host.as_ref() == Some(host))
                    .count()
            });
            same_host < 2
                && !(request.verb == "upgrade"
                    && app
                        .jobs
                        .iter()
                        .any(|job| job.verb == "upgrade" && job.host == request.host))
        });
        let Some(request) = eligible.and_then(|index| app.job_queue.remove(index)) else {
            return;
        };
        let worker_label = request.label.clone();
        let worker_verb = request.verb.clone();
        let args = request.args;
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut output = run_verbose_command("worklane", &args, &sender);
            let cached_lanes = if output.as_ref().is_ok_and(|output| output.success) {
                match load_cached_lanes() {
                    Ok(lanes) => Some(lanes),
                    Err(error) => {
                        output = Err(format!(
                            "command completed but its cached result could not be loaded: {error:#}"
                        ));
                        None
                    }
                }
            } else {
                None
            };
            let _ = sender.send(JobEvent::Complete(JobResult {
                label: worker_label,
                verb: worker_verb,
                output,
                cached_lanes,
            }));
        });
        app.jobs.push(ActiveJob {
            lane_id: request.lane_id,
            host: request.host,
            verb: request.verb,
            label: request.label.clone(),
            started: Instant::now(),
            receiver,
        });
        app.message = format!("{}: running", request.label);
        app.detail = format!("{}: starting", request.label);
    }
}
fn start_upgrade(app: &mut App, all: bool) -> Result<()> {
    if all {
        start_job(app, "upgrade", &["--no-cache".into()], true)
    } else {
        start_job(app, "upgrade", &[], false)
    }
}
fn run_verbose_command(
    program: &str,
    args: &[String],
    sender: &mpsc::Sender<JobEvent>,
) -> Result<CommandOutput, String> {
    let capture_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tmp");
    fs::create_dir_all(&capture_dir).map_err(|error| error.to_string())?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let capture = capture_dir.join(format!("lane-command-{}-{nonce}", std::process::id()));
    let stdout_path = capture.with_extension("stdout");
    let stderr_path = capture.with_extension("stderr");
    let stdout_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&stdout_path)
        .map_err(|error| error.to_string())?;
    let stderr_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&stderr_path)
        .map_err(|error| error.to_string())?;
    let mut child = Command::new(program)
        .args(args)
        .env("WORKLANE_EVENT_STREAM", "1")
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .map_err(|error| error.to_string())?;
    let status = child.wait().map_err(|error| error.to_string())?;
    let stdout = fs::read(&stdout_path).map_err(|error| error.to_string())?;
    let stderr = fs::read(&stderr_path).map_err(|error| error.to_string())?;
    let _ = fs::remove_file(&stdout_path);
    let _ = fs::remove_file(&stderr_path);
    for line in String::from_utf8_lossy(&stderr).lines() {
        let display = serde_json::from_str::<worklane_core::OperationEvent>(line).map_or_else(
            |_| line.to_string(),
            |event| {
                format!(
                    "{} · {:.1}s — {}",
                    event.phase,
                    event.elapsed_ms as f64 / 1000.0,
                    event.message
                )
            },
        );
        let _ = sender.send(JobEvent::Progress(display));
    }
    Ok(CommandOutput {
        success: status.success(),
        stdout,
        stderr,
    })
}
fn poll_jobs(app: &mut App) {
    let mut active = Vec::new();
    for job in std::mem::take(&mut app.jobs) {
        let job_lane_id = job.lane_id.clone();
        let mut complete = false;
        loop {
            match job.receiver.try_recv() {
                Ok(JobEvent::Progress(line)) => {
                    app.message = format!("{}: running", job.verb);
                    append_detail_line(app, &line);
                }
                Ok(JobEvent::Complete(result)) => {
                    let cached_lanes = result.cached_lanes;
                    let applied = (|| -> Result<()> {
                        match result.output {
                            Ok(output) if output.success => {
                                if let Some(lane_id) = &job_lane_id {
                                    app.lane_errors.remove(lane_id);
                                }
                                app.detail = match result.verb.as_str() {
                                    "upgrade" => upgrade_detail(&output.stdout)?,
                                    "diff" => {
                                        let value: serde_json::Value =
                                            serde_json::from_slice(&output.stdout)?;
                                        let lines = value["diff"]
                                            .as_array()
                                            .into_iter()
                                            .flatten()
                                            .filter_map(serde_json::Value::as_str)
                                            .collect::<Vec<_>>();
                                        if lines.is_empty() {
                                            "No meaningful writable-root changes.".into()
                                        } else {
                                            lines.join("\n")
                                        }
                                    }
                                    "inspect" => inspect_detail(&output.stdout)?,
                                    "reconcile" => {
                                        let value: serde_json::Value =
                                            serde_json::from_slice(&output.stdout)?;
                                        serde_json::to_string_pretty(&value)?
                                    }
                                    _ => String::new(),
                                };
                                app.message = format!("{}: completed", result.label);
                                if let Some(lanes) = cached_lanes {
                                    replace_lanes(app, lanes);
                                }
                            }
                            Ok(output) => {
                                app.message = format!("{}: failed", result.label);
                                app.detail = command_error(
                                    &output.stderr,
                                    &format!("{} failed without an error message", result.label),
                                );
                                if let Some(lane_id) = &job_lane_id {
                                    app.lane_errors.insert(lane_id.clone(), app.detail.clone());
                                }
                            }
                            Err(error) => {
                                app.message = format!("{}: failed", result.label);
                                app.detail = error;
                                if let Some(lane_id) = &job_lane_id {
                                    app.lane_errors.insert(lane_id.clone(), app.detail.clone());
                                }
                            }
                        }
                        Ok(())
                    })();
                    if let Err(error) = applied {
                        app.message = format!("{}: failed", result.label);
                        app.detail = format!("{error:#}");
                        if let Some(lane_id) = &job_lane_id {
                            app.lane_errors.insert(lane_id.clone(), app.detail.clone());
                        }
                    }
                    complete = true;
                    break;
                }
                Err(TryRecvError::Empty) => {
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    app.message = format!("{} worker stopped unexpectedly", job.verb);
                    app.detail = app.message.clone();
                    if let Some(lane_id) = &job_lane_id {
                        app.lane_errors.insert(lane_id.clone(), app.detail.clone());
                    }
                    complete = true;
                    break;
                }
            }
        }
        if !complete {
            active.push(job);
        }
    }
    app.jobs = active;
    start_queued_jobs(app);
}
#[cfg(test)]
fn poll_job(app: &mut App) -> Result<()> {
    poll_jobs(app);
    Ok(())
}
fn append_detail_line(app: &mut App, line: &str) {
    if !app.detail.is_empty() {
        app.detail.push('\n');
    }
    app.detail.push_str(line);
    const MAX_LINES: usize = 200;
    let line_count = app.detail.lines().count();
    if line_count > MAX_LINES {
        app.detail = app
            .detail
            .lines()
            .skip(line_count - MAX_LINES)
            .collect::<Vec<_>>()
            .join("\n");
    }
}
fn selected_has_error(app: &App) -> bool {
    app.visible()
        .get(app.selected)
        .is_some_and(|lane| app.lane_errors.contains_key(&lane.spec.id))
}
fn set_background_message(app: &mut App, message: String) {
    if !selected_has_error(app) {
        app.message = message;
    }
}
fn start_state_poll(app: &mut App) {
    if app.state_poll_pending > 0 {
        app.message = "state check already running".into();
        return;
    }
    let (sender, receiver) = mpsc::channel();
    if app.lanes.is_empty() {
        app.message = "no lanes registered".into();
        return;
    }
    app.state_poll_queue.clear();
    app.state_poll_errors.clear();
    for lane in &app.lanes {
        app.next_refresh_generation += 1;
        let generation = app.next_refresh_generation;
        app.refresh_generations
            .insert(lane.spec.id.clone(), generation);
        app.state_poll_queue.push_back(StatePollRequest {
            lane_id: lane.spec.id.clone(),
            lane_name: lane.spec.name.clone(),
            generation,
        });
    }
    app.state_poll_pending = app.state_poll_queue.len();
    app.state_poll = Some(receiver);
    app.state_poll_sender = Some(sender);
    spawn_state_polls(app);
    set_background_message(
        app,
        format!("checking lane states: {} pending", app.state_poll_pending),
    );
}
fn spawn_state_polls(app: &mut App) {
    const MAX_STATE_POLLS: usize = 4;
    while app.state_poll_inflight < MAX_STATE_POLLS {
        let Some(request) = app.state_poll_queue.pop_front() else {
            return;
        };
        let Some(sender) = app.state_poll_sender.clone() else {
            return;
        };
        app.state_poll_inflight += 1;
        std::thread::spawn(move || {
            let output = Command::new("worklane")
                .args(["--json", "lane", "inspect", &request.lane_id, "--fast"])
                .output()
                .map_err(|error| error.to_string());
            let _ = sender.send(StatePollResult {
                lane_id: request.lane_id,
                lane_name: request.lane_name,
                generation: request.generation,
                output,
            });
        });
    }
}
fn poll_state(app: &mut App) -> Result<()> {
    let Some(receiver) = app.state_poll.take() else {
        return Ok(());
    };
    loop {
        match receiver.try_recv() {
            Ok(result) => {
                app.state_poll_inflight = app.state_poll_inflight.saturating_sub(1);
                app.state_poll_pending = app.state_poll_pending.saturating_sub(1);
                let current = app.refresh_generations.get(&result.lane_id).copied();
                if current.is_some() && current != Some(result.generation) {
                    spawn_state_polls(app);
                    if app.state_poll_pending == 0 {
                        finish_state_poll(app);
                        app.state_poll_sender = None;
                        return Ok(());
                    }
                    continue;
                }
                match result.output {
                    Ok(output) if output.status.success() => {
                        let status: LaneStatus = serde_json::from_slice(&output.stdout)?;
                        update_lane_status(app, status);
                        set_background_message(
                            app,
                            format!(
                                "checked {}; {} pending",
                                result.lane_name, app.state_poll_pending
                            ),
                        );
                    }
                    Ok(output) => {
                        app.message = format!(
                            "state check failed for {}; {} pending",
                            result.lane_name, app.state_poll_pending
                        );
                        let error = command_error(
                            &output.stderr,
                            &format!(
                                "state check exited with {} without an error message",
                                output.status
                            ),
                        );
                        record_state_poll_error(app, &result.lane_id, &result.lane_name, &error);
                    }
                    Err(error) => {
                        app.message = format!(
                            "state check failed for {}; {} pending",
                            result.lane_name, app.state_poll_pending
                        );
                        record_state_poll_error(app, &result.lane_id, &result.lane_name, &error);
                    }
                }
                spawn_state_polls(app);
                if app.state_poll_pending == 0 {
                    finish_state_poll(app);
                    app.state_poll_sender = None;
                    return Ok(());
                }
            }
            Err(TryRecvError::Empty) => {
                app.state_poll = Some(receiver);
                return Ok(());
            }
            Err(TryRecvError::Disconnected) => {
                if app.state_poll_pending == 0 {
                    finish_state_poll(app);
                } else {
                    let error = format!("stopped with {} pending", app.state_poll_pending);
                    app.message = format!("state checker {error}");
                    app.state_poll_errors
                        .push(format!("state checker: {error}"));
                    app.detail = app.state_poll_errors.join("\n");
                }
                app.state_poll_sender = None;
                return Ok(());
            }
        }
    }
}
fn record_state_poll_error(app: &mut App, lane_id: &str, lane: &str, error: &str) {
    app.state_poll_errors.push(format!("{lane}: {error}"));
    app.lane_errors.insert(lane_id.into(), error.into());
    app.detail = app.state_poll_errors.join("\n");
}
fn finish_state_poll(app: &mut App) {
    let message = if app.state_poll_errors.is_empty() {
        "state checks complete".into()
    } else {
        format!(
            "state checks complete with {} failure{}; see details",
            app.state_poll_errors.len(),
            if app.state_poll_errors.len() == 1 {
                ""
            } else {
                "s"
            }
        )
    };
    if app.state_poll_errors.is_empty() {
        set_background_message(app, message);
    } else {
        app.message = message;
    }
}
fn update_lane_status(app: &mut App, status: LaneStatus) {
    let selected_id = selected_lane_id(app);
    if let Some(lane) = app
        .lanes
        .iter_mut()
        .find(|lane| lane.spec.id == status.spec.id)
    {
        *lane = status;
    }
    restore_selection(app, selected_id.as_deref());
}
fn refresh_all(app: &mut App) {
    match sync_registered_lanes(app) {
        Ok(_) => start_state_poll(app),
        Err(error) => app.message = format!("registry refresh failed: {error:#}"),
    }
}
fn edit_containerfile(app: &mut App) -> Result<()> {
    restore_terminal_state();
    let result = Command::new("worklane").args(["image", "edit"]).status();
    execute!(io::stdout(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    FORCE_FULL_REDRAW.store(true, Ordering::Release);
    let status = result?;
    app.message = format!(
        "Containerfile editor: {}",
        if status.success() { "closed" } else { "failed" }
    );
    Ok(())
}
fn restore_terminal_state() {
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(
        stdout,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
}
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal_state();
    }
}
struct ResumeTui;
impl Drop for ResumeTui {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), EnterAlternateScreen);
        let _ = enable_raw_mode();
        FORCE_FULL_REDRAW.store(true, Ordering::Release);
    }
}
fn main() {
    install_panic_hook();
    if let Err(error) = run_tui() {
        restore_terminal_state();
        eprintln!("lane: {error:#}");
        std::process::exit(1);
    }
}
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal_state();
        previous(info);
    }));
}
fn run_tui() -> Result<()> {
    restore_terminal_state();
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal);
    terminal.show_cursor()?;
    result
}
fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = App::load()?;
    run_app(
        &mut app,
        |app| {
            if FORCE_FULL_REDRAW.swap(false, Ordering::AcqRel) {
                terminal.clear()?;
            }
            Ok(terminal.draw(|frame| draw(frame, app)).map(|_| ())?)
        },
        || {
            if !event::poll(Duration::from_millis(250))? {
                return Ok(None);
            }
            Ok(match event::read()? {
                Event::Key(key) => Some(key.code),
                _ => None,
            })
        },
    )
}

fn run_app(
    app: &mut App,
    mut redraw: impl FnMut(&App) -> Result<()>,
    mut next_key: impl FnMut() -> Result<Option<KeyCode>>,
) -> Result<()> {
    let mut last_registry_sync = Instant::now();
    loop {
        poll_jobs(app);
        poll_state(app)?;
        if last_registry_sync.elapsed() >= Duration::from_secs(1) {
            match sync_registered_lanes(app) {
                Ok(true) if app.state_poll_pending == 0 => start_state_poll(app),
                Ok(_) => {}
                Err(error) => app.message = format!("registry refresh failed: {error:#}"),
            }
            last_registry_sync = Instant::now();
        }
        if let Some(job) = app.jobs.first() {
            app.message = format!(
                "{}: running {:.1}s ({} active)",
                job.label,
                job.started.elapsed().as_secs_f32(),
                app.jobs.len()
            );
        }
        redraw(app)?;
        let Some(key) = next_key()? else {
            continue;
        };
        match app.handle_key(key) {
            UiAction::Quit => break,
            UiAction::Command(verb, true) => {
                app.message = format!("{verb}: connecting");
                redraw(app)?;
                if action_with_args(app, verb, &["--shell"])? {
                    start_state_poll(app);
                }
            }
            UiAction::Command("upgrade", false) => start_upgrade(app, false)?,
            UiAction::Command(verb, false) => {
                if verb == "attach" {
                    app.message = "attach: connecting".into();
                    redraw(app)?;
                    if action(app, verb)? {
                        start_state_poll(app);
                    }
                } else {
                    start_job(app, verb, &[], false)?;
                }
            }
            UiAction::Rename(name) => start_job(app, "rename", &[name], false)?,
            UiAction::Reload => {
                refresh_all(app);
            }
            UiAction::UpgradeAll => start_upgrade(app, true)?,
            UiAction::EditContainerfile => {
                app.message = "Containerfile editor: opening".into();
                redraw(app)?;
                edit_containerfile(app)?;
            }
            UiAction::Reconcile(apply) => {
                let extra = if apply {
                    vec!["--apply".into()]
                } else {
                    Vec::new()
                };
                start_job(app, "reconcile", &extra, false)?;
            }
            UiAction::None => {}
        }
    }
    Ok(())
}
fn column(value: &str, width: usize) -> String {
    if value.chars().count() > width {
        format!(
            "{:<width$}",
            format!("{}~", value.chars().take(width - 1).collect::<String>())
        )
    } else {
        format!("{value:<width$}")
    }
}
fn lane_style(state: &str, drift: bool) -> Style {
    // Eight hues, each 45° apart around the HSL color wheel. Keeping this
    // fixed makes a lane's lifecycle state recognizable at a glance.
    let palette = [
        Color::Rgb(230, 69, 69),  // red
        Color::Rgb(230, 190, 69), // amber
        Color::Rgb(149, 230, 69), // lime
        Color::Rgb(69, 230, 109), // green
        Color::Rgb(69, 230, 230), // cyan
        Color::Rgb(69, 109, 230), // blue
        Color::Rgb(149, 69, 230), // violet
        Color::Rgb(230, 69, 190), // magenta
    ];
    let color = palette[match state {
        "exited" => 0,
        "stopped" => 1,
        "other" => 2,
        "running" => 3,
        "starting" => 4,
        "created" => 5,
        "absent" => 6,
        "unknown" => 7,
        _ => 2,
    }];
    let style = Style::default().fg(color);
    if drift {
        style.add_modifier(Modifier::UNDERLINED | Modifier::BOLD)
    } else {
        style
    }
}
fn live_age(status: &LaneStatus) -> String {
    if status.state != "running" {
        return "-".into();
    }
    let Some(started_at) = status.runtime_started_at else {
        return "…".into();
    };
    let seconds = (chrono::Utc::now() - started_at).num_seconds().max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}
fn visible_detail(app: &App) -> &str {
    selected_lane_id(app)
        .and_then(|lane_id| app.lane_errors.get(&lane_id))
        .map(String::as_str)
        .unwrap_or(&app.detail)
}
fn draw(f: &mut ratatui::Frame, app: &App) {
    let detail = visible_detail(app);
    let detail_height = if detail.is_empty() { 0 } else { 6 };
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(2),
            Constraint::Length(3),
            Constraint::Length(detail_height),
            Constraint::Length(4),
        ])
        .split(f.area());
    let mut items = vec![ListItem::new(format!(
        "{} {} {} {} {}",
        column("NAME", 18),
        column("HOST", 14),
        column("STATE", 10),
        column("ALIVE", 7),
        "IMAGE"
    ))
    .style(Style::default().fg(Color::DarkGray))];
    items.extend(app.visible().iter().enumerate().map(|(i, x)| {
        let flag = if x.drift { " drift" } else { "" };
        ListItem::new(format!(
            "{} {} {} {} {}{}",
            column(&x.spec.name, 18),
            column(&x.spec.host, 14),
            column(&x.state, 10),
            column(&live_age(x), 7),
            x.spec.profile.image,
            flag
        ))
        .style(if i == app.selected {
            lane_style(&x.state, x.drift)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            lane_style(&x.state, x.drift)
        })
    }));
    f.render_widget(
        List::new(items).block(
            Block::default()
                .title(" Worklane lanes ")
                .borders(Borders::ALL),
        ),
        areas[0],
    );
    let input_line = if let Some(name) = &app.rename_input {
        format!("Rename (typing; Enter saves, Esc cancels): {name}")
    } else {
        format!(
            "Filter{}: {}",
            if app.filtering {
                " (typing; Enter keeps, Esc clears)"
            } else {
                " (/ to edit)"
            },
            app.filter
        )
    };
    f.render_widget(
        Paragraph::new(input_line).block(Block::default().borders(Borders::ALL)),
        areas[1],
    );
    if !detail.is_empty() {
        f.render_widget(
            Paragraph::new(detail).block(
                Block::default()
                    .title(" Details — Esc closes ")
                    .borders(Borders::ALL),
            ),
            areas[2],
        );
    }
    f.render_widget(
        Paragraph::new(format!(
            "{}\n\
             j/k  /  Esc q  a/S attach  s/x run  n rename  d diff  D del  F forget  i info  e edit  u/U up  r c/C fix",
            if app.message.is_empty() {
                "ready"
            } else {
                &app.message
            },
        ))
        .block(Block::default().title(" Controls ").borders(Borders::ALL)),
        areas[3],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::{
        env, fs,
        path::PathBuf,
        sync::{Mutex, OnceLock},
    };
    use worklane_core::{LaneSpec, Profile};
    fn app() -> App {
        let spec = LaneSpec::new(
            "alpha".into(),
            "lab".into(),
            PathBuf::from("/tmp"),
            Profile::default(),
        )
        .unwrap();
        App {
            lanes: vec![LaneStatus {
                spec,
                state: "running".into(),
                drift: false,
                cached_at: Utc::now(),
                runtime_started_at: None,
            }],
            selected: 0,
            filter: String::new(),
            filtering: false,
            detail: String::new(),
            lane_errors: HashMap::new(),
            message: String::new(),
            confirming_delete: false,
            confirming_forget: false,
            confirming_reconcile: false,
            rename_input: None,
            jobs: Vec::new(),
            job_queue: VecDeque::new(),
            state_poll: None,
            state_poll_sender: None,
            state_poll_queue: VecDeque::new(),
            state_poll_inflight: 0,
            state_poll_pending: 0,
            state_poll_errors: Vec::new(),
            refresh_generations: HashMap::new(),
            next_refresh_generation: 0,
        }
    }

    fn environment_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
    fn inject_job(app: &mut App, receiver: Receiver<JobEvent>, verb: &str) {
        app.jobs.push(ActiveJob {
            lane_id: Some(app.lanes[0].spec.id.clone()),
            host: Some(app.lanes[0].spec.host.clone()),
            verb: verb.into(),
            label: verb.into(),
            started: Instant::now(),
            receiver,
        });
    }
    #[test]
    fn selection_helpers_track_identity_across_registry_updates() {
        let mut app = app();
        let started_at = Utc::now() - chrono::Duration::minutes(3);
        app.lanes[0].state = "running".into();
        app.lanes[0].runtime_started_at = Some(started_at);
        let selected_id = app.lanes[0].spec.id.clone();
        let mut second = app.lanes[0].clone();
        second.spec = LaneSpec::new(
            "beta".into(),
            "lab".into(),
            PathBuf::from("/tmp/beta"),
            Profile::default(),
        )
        .unwrap();
        second.spec.last_attached = Some(Utc::now());
        let mut first = app.lanes[0].clone();
        first.runtime_started_at = None;
        replace_lanes(&mut app, vec![first, second]);
        assert_eq!(
            selected_lane_id(&app).as_deref(),
            Some(selected_id.as_str())
        );

        app.selected = 99;
        restore_selection(&mut app, Some("missing-id"));
        assert_eq!(app.selected, app.visible().len() - 1);
        assert_eq!(
            app.lanes
                .iter()
                .find(|lane| lane.spec.id == selected_id)
                .and_then(|lane| lane.runtime_started_at),
            Some(started_at)
        );
        assert_eq!(command_error(b"", "fallback"), "fallback");
        assert_eq!(
            command_error(b" explicit error \n", "fallback"),
            "explicit error"
        );
    }
    #[test]
    fn reducer_covers_navigation_filter_and_actions() {
        let mut app = app();
        assert_eq!(app.handle_key(KeyCode::Char('/')), UiAction::None);
        assert!(app.filtering);
        app.handle_key(KeyCode::Char('a'));
        assert_eq!(app.filter, "a");
        assert_eq!(app.handle_key(KeyCode::Enter), UiAction::None);
        assert!(!app.filtering);
        assert_eq!(
            app.handle_key(KeyCode::Char('a')),
            UiAction::Command("attach", false)
        );
        assert_eq!(
            app.handle_key(KeyCode::Char('S')),
            UiAction::Command("attach", true)
        );
        assert_eq!(
            app.handle_key(KeyCode::Char('s')),
            UiAction::Command("start", false)
        );
        assert_eq!(
            app.handle_key(KeyCode::Char('x')),
            UiAction::Command("stop", false)
        );
        assert_eq!(
            app.handle_key(KeyCode::Char('u')),
            UiAction::Command("upgrade", false)
        );
        assert_eq!(app.handle_key(KeyCode::Char('U')), UiAction::UpgradeAll);
        assert_eq!(
            app.handle_key(KeyCode::Char('i')),
            UiAction::Command("inspect", false)
        );
        assert_eq!(
            app.handle_key(KeyCode::Char('d')),
            UiAction::Command("diff", false)
        );
        assert_eq!(app.handle_key(KeyCode::Char('n')), UiAction::None);
        assert_eq!(app.rename_input.as_deref(), Some(""));
        app.handle_key(KeyCode::Char('b'));
        app.handle_key(KeyCode::Char('e'));
        app.handle_key(KeyCode::Backspace);
        assert_eq!(app.handle_key(KeyCode::Enter), UiAction::Rename("b".into()));
        assert!(app.rename_input.is_none());
        app.handle_key(KeyCode::Char('n'));
        assert_eq!(app.handle_key(KeyCode::Enter), UiAction::None);
        assert_eq!(app.handle_key(KeyCode::F(2)), UiAction::None);
        assert_eq!(app.handle_key(KeyCode::Esc), UiAction::None);
        assert!(app.rename_input.is_none());
        assert_eq!(app.handle_key(KeyCode::Char('D')), UiAction::None);
        assert!(app.confirming_delete);
        assert!(app.message.contains("project /tmp is preserved"));
        assert_eq!(app.handle_key(KeyCode::Char('?')), UiAction::None);
        assert_eq!(app.handle_key(KeyCode::Char('n')), UiAction::None);
        assert!(!app.confirming_delete);
        assert_eq!(app.handle_key(KeyCode::Char('D')), UiAction::None);
        assert_eq!(
            app.handle_key(KeyCode::Char('y')),
            UiAction::Command("delete", false)
        );
        assert_eq!(app.handle_key(KeyCode::Char('F')), UiAction::None);
        assert!(app.confirming_forget);
        assert!(app.message.contains("Container, manifest"));
        assert_eq!(app.handle_key(KeyCode::Char('?')), UiAction::None);
        assert_eq!(app.handle_key(KeyCode::Esc), UiAction::None);
        assert!(!app.confirming_forget);
        assert_eq!(app.handle_key(KeyCode::Char('F')), UiAction::None);
        assert_eq!(
            app.handle_key(KeyCode::Char('y')),
            UiAction::Command("forget", false)
        );
        app.detail = "details".into();
        assert_eq!(app.handle_key(KeyCode::Esc), UiAction::None);
        assert!(app.detail.is_empty());
        assert_eq!(app.handle_key(KeyCode::Char('r')), UiAction::Reload);
        assert_eq!(
            app.handle_key(KeyCode::Char('c')),
            UiAction::Reconcile(false)
        );
        assert_eq!(app.handle_key(KeyCode::Char('C')), UiAction::None);
        assert!(app.confirming_reconcile);
        assert!(app.message.contains("Apply verified reconciliation"));
        assert_eq!(app.handle_key(KeyCode::Char('?')), UiAction::None);
        assert_eq!(app.handle_key(KeyCode::Char('n')), UiAction::None);
        assert!(!app.confirming_reconcile);
        assert_eq!(app.handle_key(KeyCode::Char('C')), UiAction::None);
        assert_eq!(
            app.handle_key(KeyCode::Char('y')),
            UiAction::Reconcile(true)
        );
        assert_eq!(
            app.handle_key(KeyCode::Char('e')),
            UiAction::EditContainerfile
        );
        assert_eq!(app.handle_key(KeyCode::Char('q')), UiAction::Quit);
        assert_eq!(app.handle_key(KeyCode::Char('?')), UiAction::None);
        app.handle_key(KeyCode::Char('/'));
        app.handle_key(KeyCode::Char('z'));
        assert_eq!(app.filter, "z");
        app.handle_key(KeyCode::Backspace);
        assert!(app.filter.is_empty());
        assert_eq!(app.handle_key(KeyCode::F(3)), UiAction::None);
        app.handle_key(KeyCode::Esc);
        assert!(app.filter.is_empty());
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected, 0);
        app.handle_key(KeyCode::Up);
        assert_eq!(app.selected, 0);
        assert_eq!(app.handle_key(KeyCode::F(1)), UiAction::None);
        assert_eq!(
            lane_style("running", false).fg,
            Some(Color::Rgb(69, 230, 109))
        );
        assert_eq!(
            lane_style("exited", false).fg,
            Some(Color::Rgb(230, 69, 69))
        );
        assert_eq!(
            lane_style("unknown", true).fg,
            Some(Color::Rgb(230, 69, 190))
        );
        assert_eq!(
            upgrade_detail(br#"[]"#).unwrap(),
            "No lanes were selected for upgrade."
        );
        assert_eq!(
            upgrade_detail(
                br#"[{"lane_id":"00000000-0000-4000-8000-000000000001","lane_name":"alpha","outcome":"skipped"}]"#,
            )
            .unwrap(),
            "alpha: skipped"
        );
        assert!(
            upgrade_detail(br#"[{"spec":{"name":"alpha"},"state":"running","drift":true}]"#)
                .unwrap()
                .contains("(drift)")
        );
    }

    #[test]
    fn lanes_sort_by_most_recent_successful_attach() {
        let mut app = app();
        let mut older = app.lanes[0].clone();
        older.spec.name = "older".into();
        older.spec.last_attached = Some(Utc::now() - chrono::Duration::minutes(1));
        app.lanes[0].spec.name = "newer".into();
        app.lanes[0].spec.last_attached = Some(Utc::now());
        let mut never = app.lanes[0].clone();
        never.spec.name = "never".into();
        never.spec.last_attached = None;
        app.lanes.extend([older, never]);
        sort_lanes(&mut app.lanes);
        assert_eq!(
            app.lanes
                .iter()
                .map(|lane| lane.spec.name.as_str())
                .collect::<Vec<_>>(),
            vec!["newer", "older", "never"]
        );
    }

    #[test]
    fn registered_lanes_render_cached_state_until_polled() {
        let _guard = environment_lock().lock().unwrap();
        let root = env::temp_dir().join(format!("lane-ui-unknown-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let previous_data = env::var_os("XDG_DATA_HOME");
        env::set_var("XDG_DATA_HOME", root.join("data"));
        let mut app = app();
        let store = Store::open_default().unwrap();
        store
            .save_lane(&app.lanes[0].spec, "running", true)
            .unwrap();

        let lanes = load_registered_lanes().unwrap();
        assert_eq!(lanes[0].state, "running");
        assert!(lanes[0].drift);

        let discovered = LaneSpec::new(
            "created-elsewhere".into(),
            "local".into(),
            root.join("created-elsewhere"),
            Profile::default(),
        )
        .unwrap();
        store.save_lane(&discovered, "running", false).unwrap();
        assert!(sync_registered_lanes(&mut app).unwrap());
        assert!(app
            .lanes
            .iter()
            .any(|lane| lane.spec.name == "created-elsewhere"));

        if let Some(value) = previous_data {
            env::set_var("XDG_DATA_HOME", value);
        } else {
            env::remove_var("XDG_DATA_HOME");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_poll_updates_lanes_individually() {
        let mut app = app();
        let beta = LaneStatus {
            spec: LaneSpec::new(
                "beta".into(),
                "lab".into(),
                PathBuf::from("/tmp"),
                Profile::default(),
            )
            .unwrap(),
            state: "unknown".into(),
            drift: false,
            cached_at: Utc::now(),
            runtime_started_at: None,
        };
        app.lanes[0].state = "unknown".into();
        app.lanes.push(beta.clone());
        app.state_poll_pending = 2;
        let (sender, receiver) = mpsc::channel();
        app.state_poll = Some(receiver);

        let mut beta_running = beta.clone();
        beta_running.state = "running".into();
        let output = Command::new("printf")
            .arg("%s")
            .arg(serde_json::to_string(&beta_running).unwrap())
            .output()
            .unwrap();
        sender
            .send(StatePollResult {
                lane_id: beta.spec.id.clone(),
                lane_name: beta.spec.name.clone(),
                generation: 0,
                output: Ok(output),
            })
            .unwrap();

        poll_state(&mut app).unwrap();
        assert_eq!(app.state_poll_pending, 1);
        assert!(app.state_poll.is_some());
        assert_eq!(
            app.lanes
                .iter()
                .find(|lane| lane.spec.name == "alpha")
                .unwrap()
                .state,
            "unknown"
        );
        assert_eq!(
            app.lanes
                .iter()
                .find(|lane| lane.spec.name == "beta")
                .unwrap()
                .state,
            "running"
        );

        let failed_output = Command::new("sh")
            .args(["-c", "printf failure >&2; exit 1"])
            .output()
            .unwrap();
        sender
            .send(StatePollResult {
                lane_id: app.lanes[0].spec.id.clone(),
                lane_name: app.lanes[0].spec.name.clone(),
                generation: 0,
                output: Ok(failed_output),
            })
            .unwrap();
        poll_state(&mut app).unwrap();
        assert_eq!(
            app.message,
            "state checks complete with 1 failure; see details"
        );
        assert_eq!(app.detail, "alpha: failure");

        let (sender, receiver) = mpsc::channel();
        app.state_poll = Some(receiver);
        app.state_poll_pending = 1;
        app.state_poll_errors.clear();
        sender
            .send(StatePollResult {
                lane_id: beta.spec.id.clone(),
                lane_name: beta.spec.name.clone(),
                generation: 0,
                output: Err("offline".into()),
            })
            .unwrap();
        poll_state(&mut app).unwrap();
        assert_eq!(
            app.message,
            "state checks complete with 1 failure; see details"
        );
        assert_eq!(app.detail, "beta: offline");

        let (sender, receiver) = mpsc::channel();
        app.state_poll = Some(receiver);
        app.state_poll_pending = 1;
        app.state_poll_errors.clear();
        drop(sender);
        poll_state(&mut app).unwrap();
        assert!(app.message.contains("stopped with 1 pending"));
        assert!(app.detail.contains("state checker: stopped with 1 pending"));

        let (sender, receiver) = mpsc::channel::<StatePollResult>();
        app.state_poll = Some(receiver);
        app.state_poll_pending = 0;
        app.state_poll_errors.clear();
        drop(sender);
        poll_state(&mut app).unwrap();
        assert!(app.message.contains("stopped with 1 pending"));
    }

    #[test]
    fn actions_reload_the_cached_store_and_run_worklane() {
        let _guard = environment_lock().lock().unwrap();
        let root = env::temp_dir().join(format!("lane-ui-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let worklane = bin.join("worklane");
        fs::write(
            &worklane,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$0.log\"\nstatus='{\"spec\":{\"schema_version\":5,\"id\":\"00000000-0000-4000-8000-000000000006\",\"name\":\"alpha\",\"container_name\":\"worklane-alpha-00000000-0000-4000-8000-000000000006\",\"session_name\":\"alpha\",\"container_home\":\"/home/dev\",\"host\":\"lab\",\"project_path\":\"/tmp\",\"profile\":{\"image\":\"test:latest\",\"build_context\":\"project\",\"containerfile\":\"Containerfile\",\"embedded_containerfile\":true,\"network\":\"outbound\",\"mounts\":[],\"mount_codex_credentials\":true,\"mount_gh_credentials\":true},\"profile_name\":\"default\",\"created_at\":\"2026-01-01T00:00:00Z\",\"last_attached\":null,\"image_digest\":null},\"state\":\"running\",\"drift\":false,\"cached_at\":\"2026-01-01T00:00:00Z\"}'\ncase \"$*\" in *fail*) printf 'command failed\\n' >&2; exit 1;; *diff*) printf '{\"diff\":[]}\\n';; *inspect*) printf '%s\\n' \"$status\";; *upgrade*) printf 'building alpha\\n' >&2; printf '[%s]\\n' \"$status\";; *refresh*) printf '[%s]\\n' \"$status\";; esac\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&worklane, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let original_data = env::var_os("XDG_DATA_HOME");
        if original_data.is_none() {
            env::set_var("XDG_DATA_HOME", root.join("outer-data"));
        }
        let previous_data = env::var_os("XDG_DATA_HOME");
        let previous_path = env::var_os("PATH");
        env::set_var("XDG_DATA_HOME", root.join("data"));
        env::set_var(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                previous_path.as_deref().unwrap().to_string_lossy()
            ),
        );
        let mut app = app();
        Store::open_default()
            .unwrap()
            .save_lane(&app.lanes[0].spec, "running", false)
            .unwrap();
        assert!(action(&mut app, "start").unwrap());
        assert_eq!(app.message, "start: ok");
        action_with_args(&mut app, "attach", &["--shell"]).unwrap();
        assert_eq!(app.message, "attach: ok");
        action(&mut app, "diff").unwrap();
        assert_eq!(app.detail, "No meaningful writable-root changes.");
        action(&mut app, "inspect").unwrap();
        assert!(app.detail.contains("State: running"));
        action(&mut app, "upgrade").unwrap();
        assert_eq!(app.detail, "alpha: running");
        assert!(!action(&mut app, "fail").unwrap());
        assert_eq!(app.message, "fail: failed");
        assert_eq!(app.detail, "command failed");
        start_upgrade(&mut app, true).unwrap();
        for _ in 0..100 {
            poll_job(&mut app).unwrap();
            if app.jobs.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.message, "upgrade all: completed");
        assert!(app.detail.contains("alpha: running"));
        assert!(fs::read_to_string(worklane.with_extension("log"))
            .unwrap()
            .lines()
            .any(|line| line == "lane upgrade --all --no-cache"));
        start_job(&mut app, "rename", &["renamed".into()], false).unwrap();
        start_job(&mut app, "start", &[], false).unwrap();
        assert_eq!(app.message, "start: already queued or running");
        for _ in 0..100 {
            poll_job(&mut app).unwrap();
            if app.jobs.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.message, "rename: completed");

        let (sender, receiver) = mpsc::channel();
        inject_job(&mut app, receiver, "diff");
        sender.send(JobEvent::Progress("working".into())).unwrap();
        sender
            .send(JobEvent::Complete(JobResult {
                label: "diff".into(),
                verb: "diff".into(),
                output: Ok(CommandOutput {
                    success: true,
                    stdout: br#"{"diff":["A /opt/result"]}"#.to_vec(),
                    stderr: vec![],
                }),
                cached_lanes: None,
            }))
            .unwrap();
        poll_job(&mut app).unwrap();
        assert_eq!(app.detail, "A /opt/result");

        let (sender, receiver) = mpsc::channel();
        inject_job(&mut app, receiver, "diff");
        sender
            .send(JobEvent::Complete(JobResult {
                label: "diff".into(),
                verb: "diff".into(),
                output: Ok(CommandOutput {
                    success: true,
                    stdout: br#"{"diff":[]}"#.to_vec(),
                    stderr: vec![],
                }),
                cached_lanes: None,
            }))
            .unwrap();
        poll_job(&mut app).unwrap();
        assert_eq!(app.detail, "No meaningful writable-root changes.");

        let (sender, receiver) = mpsc::channel();
        inject_job(&mut app, receiver, "inspect");
        sender
            .send(JobEvent::Complete(JobResult {
                label: "inspect".into(),
                verb: "inspect".into(),
                output: Ok(CommandOutput {
                    success: true,
                    stdout: br#"{"spec":{"name":"alpha","host":"lab","project_path":"/tmp","profile":{"image":"test"}},"state":"running","drift":true}"#.to_vec(),
                    stderr: vec![],
                }),
                cached_lanes: None,
            }))
            .unwrap();
        poll_job(&mut app).unwrap();
        assert!(app.detail.contains("(drift)"));

        let (sender, receiver) = mpsc::channel();
        inject_job(&mut app, receiver, "start");
        sender
            .send(JobEvent::Complete(JobResult {
                label: "start".into(),
                verb: "start".into(),
                output: Ok(CommandOutput {
                    success: false,
                    stdout: vec![],
                    stderr: b"runtime failed".to_vec(),
                }),
                cached_lanes: None,
            }))
            .unwrap();
        poll_job(&mut app).unwrap();
        assert_eq!(app.detail, "runtime failed");

        let (sender, receiver) = mpsc::channel();
        inject_job(&mut app, receiver, "stop");
        sender
            .send(JobEvent::Complete(JobResult {
                label: "stop".into(),
                verb: "stop".into(),
                output: Err("spawn failed".into()),
                cached_lanes: None,
            }))
            .unwrap();
        poll_job(&mut app).unwrap();
        assert_eq!(app.detail, "spawn failed");

        let (sender, receiver) = mpsc::channel::<JobEvent>();
        inject_job(&mut app, receiver, "test");
        drop(sender);
        poll_job(&mut app).unwrap();
        assert_eq!(app.message, "test worker stopped unexpectedly");
        app.detail.clear();
        for index in 0..205 {
            append_detail_line(&mut app, &format!("line {index}"));
        }
        assert_eq!(app.detail.lines().count(), 200);
        let mut loaded = App::load().unwrap();
        for _ in 0..100 {
            poll_state(&mut loaded).unwrap();
            if loaded.state_poll.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(loaded.lanes.len(), 1);
        let mut keys = vec![
            None,
            Some(KeyCode::Esc),
            Some(KeyCode::Char('r')),
            Some(KeyCode::Char('q')),
        ]
        .into_iter();
        let mut redraws = 0;
        run_app(
            &mut app,
            |_| {
                redraws += 1;
                Ok(())
            },
            || Ok(keys.next().expect("test has a key for every redraw")),
        )
        .unwrap();
        assert_eq!(redraws, 4);
        assert!(
            app.message.starts_with("checking lane states:")
                || app.message == "state checks complete",
            "{}",
            app.message
        );
        for _ in 0..200 {
            poll_job(&mut app).unwrap();
            if app.jobs.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.jobs.is_empty());
        let (sender, receiver) = mpsc::channel();
        inject_job(&mut app, receiver, "start");
        let _sender = sender;
        let mut locked_keys = vec![Some(KeyCode::Char('s')), Some(KeyCode::Char('q'))].into_iter();
        let mut locked_redraws = 0;
        run_app(
            &mut app,
            |_| {
                locked_redraws += 1;
                Ok(())
            },
            || Ok(locked_keys.next().expect("test has a key for every redraw")),
        )
        .unwrap();
        assert_eq!(locked_redraws, 2);
        assert!(app.message.contains("queued") || app.message.contains("running"));
        app.jobs.clear();
        app.job_queue.clear();
        let failing_bin = root.join("failing-bin");
        fs::create_dir_all(&failing_bin).unwrap();
        let replacement = failing_bin.join("worklane");
        fs::write(
            &replacement,
            "#!/bin/sh\nprintf 'attach exploded\\n' >&2\nexit 17\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).unwrap();
        }
        env::set_var(
            "PATH",
            format!(
                "{}:{}",
                failing_bin.display(),
                env::var_os("PATH").unwrap().to_string_lossy()
            ),
        );
        let mut attach_keys = vec![Some(KeyCode::Char('a')), Some(KeyCode::Char('q'))].into_iter();
        let mut attach_redraws = 0;
        run_app(
            &mut app,
            |_| {
                attach_redraws += 1;
                Ok(())
            },
            || {
                Ok(attach_keys
                    .next()
                    .expect("failed attach must redraw before quit"))
            },
        )
        .unwrap();
        assert_eq!(attach_redraws, 3);
        assert_eq!(app.message, "attach: failed");
        assert_eq!(app.detail, "attach exploded");
        let mut empty = App {
            lanes: vec![],
            selected: 0,
            filter: String::new(),
            filtering: false,
            detail: String::new(),
            lane_errors: HashMap::new(),
            message: String::new(),
            confirming_delete: false,
            confirming_forget: false,
            confirming_reconcile: false,
            rename_input: None,
            jobs: Vec::new(),
            job_queue: VecDeque::new(),
            state_poll: None,
            state_poll_sender: None,
            state_poll_queue: VecDeque::new(),
            state_poll_inflight: 0,
            state_poll_pending: 0,
            state_poll_errors: Vec::new(),
            refresh_generations: HashMap::new(),
            next_refresh_generation: 0,
        };
        action(&mut empty, "start").unwrap();
        start_job(&mut empty, "start", &[], false).unwrap();
        if let Some(value) = previous_data {
            env::set_var("XDG_DATA_HOME", value);
        } else {
            env::remove_var("XDG_DATA_HOME");
        }
        if let Some(value) = previous_path {
            env::set_var("PATH", value);
        }
        if original_data.is_none() {
            env::remove_var("XDG_DATA_HOME");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verbose_commands_do_not_wait_for_descendants_holding_standard_streams() {
        let (sender, _receiver) = mpsc::channel();
        let started = Instant::now();
        let output = run_verbose_command(
            "sh",
            &["-c".into(), "sleep 1 & printf done".into()],
            &sender,
        )
        .unwrap();
        assert!(output.success);
        assert_eq!(output.stdout, b"done");
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn renderer_draws_lane_status() {
        let mut app = app();
        app.lanes[0].runtime_started_at = Some(Utc::now() - chrono::Duration::seconds(12));
        assert!(live_age(&app.lanes[0]).ends_with('s'));
        app.lanes[0].runtime_started_at = Some(Utc::now() - chrono::Duration::minutes(12));
        assert_eq!(live_age(&app.lanes[0]), "12m");
        app.lanes[0].runtime_started_at = Some(Utc::now() - chrono::Duration::minutes(61));
        assert_eq!(live_age(&app.lanes[0]), "1h");
        app.lanes[0].runtime_started_at = Some(Utc::now() - chrono::Duration::days(2));
        assert_eq!(live_age(&app.lanes[0]), "2d");
        app.lanes.push(LaneStatus {
            spec: LaneSpec::new(
                "beta".into(),
                "other".into(),
                PathBuf::from("/tmp"),
                Profile::default(),
            )
            .unwrap(),
            state: "stopped".into(),
            drift: true,
            cached_at: Utc::now(),
            runtime_started_at: None,
        });
        app.selected = 1;
        app.detail = "C /etc/example".into();
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Worklane lanes"));
        assert!(text.contains("alpha"));
        assert!(text.contains("beta"));
        assert!(text.contains("drift"));
        assert!(text.contains("C /etc/example"));
        assert!(text.contains("Controls"));
        assert!(text.contains("Esc"));
        assert!(text.contains("a/S attach"));
        assert!(text.contains("n rename"));
        assert!(text.contains("D del"));
        assert!(text.contains("F forget"));
        assert!(text.contains("u/U up"));

        let inspected = inspect_detail(
            br#"{"spec":{"name":"alpha","host":"lab","project_path":"/tmp","profile":{"image":"test:latest"}},"state":"running","drift":false}"#,
        )
        .unwrap();
        assert!(inspected.contains("Home: /home/dev"));
    }
}
