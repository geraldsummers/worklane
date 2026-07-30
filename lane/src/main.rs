use anyhow::Result;
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
    io::{self, BufRead, BufReader, Read},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::Duration,
};
use worklane_core::{LaneStatus, Store};

struct App {
    lanes: Vec<LaneStatus>,
    selected: usize,
    filter: String,
    filtering: bool,
    detail: String,
    message: String,
    confirming_delete: bool,
    confirming_forget: bool,
    rename_input: Option<String>,
    job: Option<Receiver<JobEvent>>,
    state_poll: Option<Receiver<StatePollResult>>,
    state_poll_pending: usize,
}
enum JobEvent {
    Progress(String),
    Complete(JobResult),
}
struct JobResult {
    label: String,
    verb: String,
    output: Result<CommandOutput, String>,
}
struct CommandOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
struct StatePollResult {
    lane_id: String,
    lane_name: String,
    output: Result<std::process::Output, String>,
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
            message: String::new(),
            confirming_delete: false,
            confirming_forget: false,
            rename_input: None,
            job: None,
            state_poll: None,
            state_poll_pending: 0,
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
        match key {
            KeyCode::Esc if !self.detail.is_empty() => {
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
                self.message =
                    "Delete selected lane and registry? Bind-mounted data is preserved. y/N".into();
                UiAction::None
            }
            KeyCode::Char('F') if self.visible().get(self.selected).is_some() => {
                self.confirming_forget = true;
                self.message =
                    "Forget selected lane from registry only? Podman is untouched. y/N".into();
                UiAction::None
            }
            KeyCode::Char('i') => UiAction::Command("inspect", false),
            KeyCode::Char('n') if self.visible().get(self.selected).is_some() => {
                self.rename_input = Some(String::new());
                self.message = "enter a new lane name; Enter saves, Esc cancels".into();
                UiAction::None
            }
            KeyCode::Char('r') => UiAction::Reload,
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
    let mut lanes = Store::open_default()?.lanes()?;
    for lane in &mut lanes {
        lane.state = "unknown".into();
        lane.drift = false;
    }
    Ok(lanes)
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
fn action(app: &mut App, verb: &str) -> Result<()> {
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
                .or_else(|| value["lane"].as_str())
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
fn action_with_args(app: &mut App, verb: &str, extra: &[&str]) -> Result<()> {
    let visible = app.visible();
    let Some(item) = visible.get(app.selected) else {
        return Ok(());
    };
    let mut args = vec!["lane", verb, &item.spec.id];
    args.extend(extra.iter().copied());
    let mut command = Command::new("worklane");
    command.args(args);
    if verb == "attach" {
        let status = command.status()?;
        app.message = format!("{verb}: {}", if status.success() { "ok" } else { "failed" });
        return Ok(());
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
        app.lanes = load_cached_lanes()?;
        sort_lanes(&mut app.lanes);
        clamp_selection(app);
    } else {
        app.message = format!("{verb}: failed");
        app.detail = String::from_utf8_lossy(&output.stderr).trim().into();
    }
    Ok(())
}
fn start_job(app: &mut App, verb: &str, extra: &[String], all: bool) -> Result<()> {
    if app.job.is_some() {
        app.message = "another action is already running".into();
        return Ok(());
    }
    let args = if all {
        let mut args = vec!["lane".into(), verb.into(), "--all".into()];
        args.extend(extra.iter().cloned());
        args
    } else {
        let visible = app.visible();
        let Some(item) = visible.get(app.selected) else {
            return Ok(());
        };
        let mut args = vec!["lane".into(), verb.into(), item.spec.id.clone()];
        args.extend(extra.iter().cloned());
        args
    };
    let label = if all {
        format!("{verb} all")
    } else {
        verb.to_string()
    };
    let worker_label = label.clone();
    let worker_verb = verb.to_string();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let output = run_verbose_command("worklane", &args, &sender);
        let _ = sender.send(JobEvent::Complete(JobResult {
            label: worker_label,
            verb: worker_verb,
            output,
        }));
    });
    app.job = Some(receiver);
    app.message = format!("{label}: running in background");
    app.detail = format!("{label}: starting");
    Ok(())
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
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdout = child.stdout.take().ok_or("child stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("child stderr unavailable")?;
    let progress_sender = sender.clone();
    let stderr_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        for line in BufReader::new(stderr).lines() {
            match line {
                Ok(line) => {
                    bytes.extend_from_slice(line.as_bytes());
                    bytes.push(b'\n');
                    let _ = progress_sender.send(JobEvent::Progress(line));
                }
                Err(error) => {
                    let line = format!("failed to read progress: {error}");
                    bytes.extend_from_slice(line.as_bytes());
                    bytes.push(b'\n');
                    let _ = progress_sender.send(JobEvent::Progress(line));
                    break;
                }
            }
        }
        bytes
    });
    let mut stdout_bytes = Vec::new();
    stdout
        .read_to_end(&mut stdout_bytes)
        .map_err(|error| error.to_string())?;
    let status = child.wait().map_err(|error| error.to_string())?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| "progress reader panicked".to_string())?;
    Ok(CommandOutput {
        success: status.success(),
        stdout: stdout_bytes,
        stderr,
    })
}
fn poll_job(app: &mut App) -> Result<()> {
    let Some(receiver) = app.job.take() else {
        return Ok(());
    };
    loop {
        match receiver.try_recv() {
            Ok(JobEvent::Progress(line)) => {
                app.message = "action running; lane controls locked".into();
                append_detail_line(app, &line);
            }
            Ok(JobEvent::Complete(result)) => {
                match result.output {
                    Ok(output) if output.success => {
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
                            _ => String::new(),
                        };
                        app.message = format!("{}: completed", result.label);
                        app.lanes = load_cached_lanes()?;
                        sort_lanes(&mut app.lanes);
                        clamp_selection(app);
                    }
                    Ok(output) => {
                        app.message = format!("{}: failed", result.label);
                        app.detail = String::from_utf8_lossy(&output.stderr).trim().into();
                    }
                    Err(error) => {
                        app.message = format!("{}: failed", result.label);
                        app.detail = error;
                    }
                }
                return Ok(());
            }
            Err(TryRecvError::Empty) => {
                app.job = Some(receiver);
                return Ok(());
            }
            Err(TryRecvError::Disconnected) => {
                app.message = "action worker stopped unexpectedly".into();
                return Ok(());
            }
        }
    }
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
fn start_state_poll(app: &mut App) {
    if app.state_poll.is_some() {
        app.message = "state check already running".into();
        return;
    }
    let (sender, receiver) = mpsc::channel();
    let lanes = app
        .lanes
        .iter()
        .map(|lane| (lane.spec.id.clone(), lane.spec.name.clone()))
        .collect::<Vec<_>>();
    if lanes.is_empty() {
        app.message = "no lanes registered".into();
        return;
    }
    app.state_poll_pending = lanes.len();
    for (lane_id, lane_name) in lanes {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let output = Command::new("worklane")
                .args(["--json", "lane", "inspect", &lane_id, "--fast"])
                .output()
                .map_err(|error| error.to_string());
            let _ = sender.send(StatePollResult {
                lane_id,
                lane_name,
                output,
            });
        });
    }
    app.state_poll = Some(receiver);
    app.message = format!("checking lane states: {} pending", app.state_poll_pending);
}
fn poll_state(app: &mut App) -> Result<()> {
    let Some(receiver) = app.state_poll.take() else {
        return Ok(());
    };
    loop {
        match receiver.try_recv() {
            Ok(result) => {
                app.state_poll_pending = app.state_poll_pending.saturating_sub(1);
                match result.output {
                    Ok(output) if output.status.success() => {
                        let status: LaneStatus = serde_json::from_slice(&output.stdout)?;
                        update_lane_status(app, status);
                        app.message = format!(
                            "checked {}; {} pending",
                            result.lane_name, app.state_poll_pending
                        );
                        app.detail.clear();
                    }
                    Ok(output) => {
                        mark_lane_state(app, &result.lane_id, "unknown", false);
                        app.message = format!(
                            "state check failed for {}; {} pending",
                            result.lane_name, app.state_poll_pending
                        );
                        app.detail = String::from_utf8_lossy(&output.stderr).trim().into();
                    }
                    Err(error) => {
                        mark_lane_state(app, &result.lane_id, "unknown", false);
                        app.message = format!(
                            "state check failed for {}; {} pending",
                            result.lane_name, app.state_poll_pending
                        );
                        app.detail = error;
                    }
                }
                if app.state_poll_pending == 0 {
                    app.message = "state checks complete".into();
                    return Ok(());
                }
            }
            Err(TryRecvError::Empty) => {
                app.state_poll = Some(receiver);
                return Ok(());
            }
            Err(TryRecvError::Disconnected) => {
                if app.state_poll_pending == 0 {
                    app.message = "state checks complete".into();
                } else {
                    app.message = format!(
                        "state checker stopped with {} pending",
                        app.state_poll_pending
                    );
                }
                return Ok(());
            }
        }
    }
}
fn update_lane_status(app: &mut App, status: LaneStatus) {
    if let Some(lane) = app
        .lanes
        .iter_mut()
        .find(|lane| lane.spec.id == status.spec.id)
    {
        *lane = status;
    }
    sort_lanes(&mut app.lanes);
    clamp_selection(app);
}
fn mark_lane_state(app: &mut App, lane_id: &str, state: &str, drift: bool) {
    if let Some(lane) = app.lanes.iter_mut().find(|lane| lane.spec.id == lane_id) {
        lane.state = state.into();
        lane.drift = drift;
    }
}
fn refresh_all(app: &mut App) {
    if app.state_poll.is_none() {
        for lane in &mut app.lanes {
            lane.state = "unknown".into();
            lane.drift = false;
        }
        sort_lanes(&mut app.lanes);
        clamp_selection(app);
        app.detail.clear();
    }
    start_state_poll(app);
}
fn edit_containerfile(app: &mut App) -> Result<()> {
    restore_terminal_state();
    let result = Command::new("worklane").args(["image", "edit"]).status();
    execute!(io::stdout(), EnterAlternateScreen)?;
    enable_raw_mode()?;
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
fn main() -> Result<()> {
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
        |app| Ok(terminal.draw(|frame| draw(frame, app)).map(|_| ())?),
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
    loop {
        poll_job(app)?;
        poll_state(app)?;
        redraw(app)?;
        let Some(key) = next_key()? else {
            continue;
        };
        if app.job.is_some() && key != KeyCode::Char('q') {
            app.message = "action running; lane controls locked".into();
            continue;
        }
        match app.handle_key(key) {
            UiAction::Quit => break,
            UiAction::Command(verb, true) => {
                action_with_args(app, verb, &["--shell"])?;
                if verb == "attach" {
                    break;
                }
            }
            UiAction::Command("upgrade", false) => start_upgrade(app, false)?,
            UiAction::Command(verb, false) => {
                if verb == "attach" {
                    action(app, verb)?;
                    break;
                } else {
                    start_job(app, verb, &[], false)?;
                }
            }
            UiAction::Rename(name) => start_job(app, "rename", &[name], false)?,
            UiAction::Reload => {
                refresh_all(app);
            }
            UiAction::UpgradeAll => start_upgrade(app, true)?,
            UiAction::EditContainerfile => edit_containerfile(app)?,
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
fn draw(f: &mut ratatui::Frame, app: &App) {
    let detail_height = if app.detail.is_empty() { 0 } else { 6 };
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
        "{} {} {} {}",
        column("NAME", 18),
        column("HOST", 14),
        column("STATE", 10),
        "IMAGE"
    ))
    .style(Style::default().fg(Color::DarkGray))];
    items.extend(app.visible().iter().enumerate().map(|(i, x)| {
        let flag = if x.drift { " drift" } else { "" };
        ListItem::new(format!(
            "{} {} {} {}{}",
            column(&x.spec.name, 18),
            column(&x.spec.host, 14),
            column(&x.state, 10),
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
    if !app.detail.is_empty() {
        f.render_widget(
            Paragraph::new(app.detail.as_str()).block(
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
             j/k  /  Esc q  a/S attach  s/x run  n rename  d diff  D del  F forget  i info  e edit  u/U up  r",
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
            }],
            selected: 0,
            filter: String::new(),
            filtering: false,
            detail: String::new(),
            message: String::new(),
            confirming_delete: false,
            confirming_forget: false,
            rename_input: None,
            job: None,
            state_poll: None,
            state_poll_pending: 0,
        }
    }

    fn environment_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
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
        assert!(app.message.contains("Bind-mounted data is preserved"));
        assert_eq!(app.handle_key(KeyCode::Char('n')), UiAction::None);
        assert!(!app.confirming_delete);
        assert_eq!(app.handle_key(KeyCode::Char('D')), UiAction::None);
        assert_eq!(
            app.handle_key(KeyCode::Char('y')),
            UiAction::Command("delete", false)
        );
        assert_eq!(app.handle_key(KeyCode::Char('F')), UiAction::None);
        assert!(app.confirming_forget);
        assert!(app.message.contains("Podman is untouched"));
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
            app.handle_key(KeyCode::Char('e')),
            UiAction::EditContainerfile
        );
        assert_eq!(app.handle_key(KeyCode::Char('q')), UiAction::Quit);
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
            upgrade_detail(br#"[{"lane":"alpha","outcome":"skipped"}]"#).unwrap(),
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
    fn registered_lanes_start_unknown_until_polled() {
        let _guard = environment_lock().lock().unwrap();
        let root = env::temp_dir().join(format!("lane-ui-unknown-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let previous_data = env::var_os("XDG_DATA_HOME");
        env::set_var("XDG_DATA_HOME", root.join("data"));
        let app = app();
        Store::open_default()
            .unwrap()
            .save_lane(&app.lanes[0].spec, "running", true)
            .unwrap();

        let lanes = load_registered_lanes().unwrap();
        assert_eq!(lanes[0].state, "unknown");
        assert!(!lanes[0].drift);

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

        let (sender, receiver) = mpsc::channel();
        app.state_poll = Some(receiver);
        app.state_poll_pending = 1;
        let failed_output = Command::new("sh")
            .args(["-c", "printf failure >&2; exit 1"])
            .output()
            .unwrap();
        sender
            .send(StatePollResult {
                lane_id: beta.spec.id.clone(),
                lane_name: beta.spec.name.clone(),
                output: Ok(failed_output),
            })
            .unwrap();
        poll_state(&mut app).unwrap();
        assert_eq!(app.detail, "failure");

        let (sender, receiver) = mpsc::channel();
        app.state_poll = Some(receiver);
        app.state_poll_pending = 1;
        sender
            .send(StatePollResult {
                lane_id: beta.spec.id.clone(),
                lane_name: beta.spec.name.clone(),
                output: Err("offline".into()),
            })
            .unwrap();
        poll_state(&mut app).unwrap();
        assert_eq!(app.message, "state checks complete");
        assert_eq!(app.detail, "offline");

        let (sender, receiver) = mpsc::channel();
        app.state_poll = Some(receiver);
        app.state_poll_pending = 1;
        drop(sender);
        poll_state(&mut app).unwrap();
        assert!(app.message.contains("stopped with 1 pending"));

        let (sender, receiver) = mpsc::channel::<StatePollResult>();
        app.state_poll = Some(receiver);
        app.state_poll_pending = 0;
        drop(sender);
        poll_state(&mut app).unwrap();
        assert_eq!(app.message, "state checks complete");
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
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$0.log\"\nstatus='{\"spec\":{\"version\":1,\"id\":\"alpha-id\",\"name\":\"alpha\",\"host\":\"lab\",\"user\":\"dev\",\"project_path\":\"/tmp\",\"profile\":{\"image\":\"test:latest\",\"build_context\":null,\"containerfile\":\"Containerfile\",\"embedded_containerfile\":true,\"network\":\"outbound\",\"mounts\":[]},\"profile_name\":\"default\",\"created_at\":\"2026-01-01T00:00:00Z\",\"image_digest\":null},\"state\":\"running\",\"drift\":false,\"cached_at\":\"2026-01-01T00:00:00Z\"}'\ncase \"$*\" in *fail*) printf 'command failed\\n' >&2; exit 1;; *diff*) printf '{\"diff\":[]}\\n';; *inspect*) printf '%s\\n' \"$status\";; *upgrade*) printf 'building alpha\\n' >&2; printf '[%s]\\n' \"$status\";; *refresh*) printf '[%s]\\n' \"$status\";; esac\nexit 0\n",
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
        action(&mut app, "start").unwrap();
        assert_eq!(app.message, "start: ok");
        action_with_args(&mut app, "attach", &["--shell"]).unwrap();
        assert_eq!(app.message, "attach: ok");
        action(&mut app, "diff").unwrap();
        assert_eq!(app.detail, "No meaningful writable-root changes.");
        action(&mut app, "inspect").unwrap();
        assert!(app.detail.contains("State: running"));
        action(&mut app, "upgrade").unwrap();
        assert_eq!(app.detail, "alpha: running");
        action(&mut app, "fail").unwrap();
        assert_eq!(app.message, "fail: failed");
        assert_eq!(app.detail, "command failed");
        start_upgrade(&mut app, true).unwrap();
        for _ in 0..100 {
            poll_job(&mut app).unwrap();
            if app.job.is_none() {
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
        assert_eq!(app.message, "another action is already running");
        for _ in 0..100 {
            poll_job(&mut app).unwrap();
            if app.job.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.message, "rename: completed");

        let (sender, receiver) = mpsc::channel();
        app.job = Some(receiver);
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
            }))
            .unwrap();
        poll_job(&mut app).unwrap();
        assert_eq!(app.detail, "A /opt/result");

        let (sender, receiver) = mpsc::channel();
        app.job = Some(receiver);
        sender
            .send(JobEvent::Complete(JobResult {
                label: "diff".into(),
                verb: "diff".into(),
                output: Ok(CommandOutput {
                    success: true,
                    stdout: br#"{"diff":[]}"#.to_vec(),
                    stderr: vec![],
                }),
            }))
            .unwrap();
        poll_job(&mut app).unwrap();
        assert_eq!(app.detail, "No meaningful writable-root changes.");

        let (sender, receiver) = mpsc::channel();
        app.job = Some(receiver);
        sender
            .send(JobEvent::Complete(JobResult {
                label: "inspect".into(),
                verb: "inspect".into(),
                output: Ok(CommandOutput {
                    success: true,
                    stdout: br#"{"spec":{"name":"alpha","host":"lab","project_path":"/tmp","profile":{"image":"test"}},"state":"running","drift":true}"#.to_vec(),
                    stderr: vec![],
                }),
            }))
            .unwrap();
        poll_job(&mut app).unwrap();
        assert!(app.detail.contains("(drift)"));

        let (sender, receiver) = mpsc::channel();
        app.job = Some(receiver);
        sender
            .send(JobEvent::Complete(JobResult {
                label: "start".into(),
                verb: "start".into(),
                output: Ok(CommandOutput {
                    success: false,
                    stdout: vec![],
                    stderr: b"runtime failed".to_vec(),
                }),
            }))
            .unwrap();
        poll_job(&mut app).unwrap();
        assert_eq!(app.detail, "runtime failed");

        let (sender, receiver) = mpsc::channel();
        app.job = Some(receiver);
        sender
            .send(JobEvent::Complete(JobResult {
                label: "stop".into(),
                verb: "stop".into(),
                output: Err("spawn failed".into()),
            }))
            .unwrap();
        poll_job(&mut app).unwrap();
        assert_eq!(app.detail, "spawn failed");

        let (sender, receiver) = mpsc::channel::<JobEvent>();
        app.job = Some(receiver);
        drop(sender);
        poll_job(&mut app).unwrap();
        assert_eq!(app.message, "action worker stopped unexpectedly");
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
            Some(KeyCode::Backspace),
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
        let (sender, receiver) = mpsc::channel();
        app.job = Some(receiver);
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
        assert_eq!(app.message, "action running; lane controls locked");
        app.job = None;
        let mut attach_key = Some(KeyCode::Char('a'));
        let mut attach_redraws = 0;
        run_app(
            &mut app,
            |_| {
                attach_redraws += 1;
                Ok(())
            },
            || Ok(attach_key.take()),
        )
        .unwrap();
        assert_eq!(attach_redraws, 1);
        let mut empty = App {
            lanes: vec![],
            selected: 0,
            filter: String::new(),
            filtering: false,
            detail: String::new(),
            message: String::new(),
            confirming_delete: false,
            confirming_forget: false,
            rename_input: None,
            job: None,
            state_poll: None,
            state_poll_pending: 0,
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
    fn renderer_draws_lane_status() {
        let mut app = app();
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
            br#"{"spec":{"name":"alpha","host":"lab","project_path":"/tmp","user":"gerald","profile":{"image":"test:latest"}},"state":"running","drift":false}"#,
        )
        .unwrap();
        assert!(inspected.contains("Home: /home/dev"));
    }
}
