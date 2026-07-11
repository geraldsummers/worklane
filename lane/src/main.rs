use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
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
use std::{io, process::Command, time::Duration};
use worklane_core::{LaneStatus, Store};

struct App {
    lanes: Vec<LaneStatus>,
    selected: usize,
    filter: String,
    filtering: bool,
    detail: String,
    message: String,
}
#[derive(Debug, PartialEq, Eq)]
enum UiAction {
    None,
    Quit,
    Command(&'static str, bool),
    UpgradeAll,
    Reload,
}
impl App {
    fn load() -> Result<Self> {
        Ok(Self {
            lanes: Store::open_default()?.lanes()?,
            selected: 0,
            filter: String::new(),
            filtering: false,
            detail: String::new(),
            message: String::new(),
        })
    }
    fn visible(&self) -> Vec<&LaneStatus> {
        self.lanes
            .iter()
            .filter(|x| x.spec.name.contains(&self.filter) || x.spec.host.contains(&self.filter))
            .collect()
    }
    fn handle_key(&mut self, key: KeyCode) -> UiAction {
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
            KeyCode::Char('i') => UiAction::Command("inspect", false),
            KeyCode::Char('r') => UiAction::Reload,
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
        app.lanes = Store::open_default()?.lanes()?;
    } else {
        app.message = format!("{verb}: failed");
        app.detail = String::from_utf8_lossy(&output.stderr).trim().into();
    }
    Ok(())
}
fn upgrade_all(app: &mut App) -> Result<()> {
    let output = Command::new("worklane")
        .args(["lane", "upgrade", "--all"])
        .output()?;
    if output.status.success() {
        app.detail = upgrade_detail(&output.stdout)?;
        app.message = "upgrade all: completed".into();
        app.lanes = Store::open_default()?.lanes()?;
    } else {
        app.message = "upgrade all: failed".into();
        app.detail = String::from_utf8_lossy(&output.stderr).trim().into();
    }
    Ok(())
}
fn refresh_all(app: &mut App) -> Result<()> {
    let output = Command::new("worklane")
        .args(["lane", "refresh", "--all"])
        .output()?;
    if output.status.success() {
        app.lanes = Store::open_default()?.lanes()?;
        app.message = "refreshed reachable lanes".into();
        app.detail.clear();
    } else {
        app.message = "refresh: failed".into();
        app.detail = String::from_utf8_lossy(&output.stderr).trim().into();
    }
    Ok(())
}
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen);
    }
}
fn main() -> Result<()> {
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
        redraw(app)?;
        let Some(key) = next_key()? else {
            continue;
        };
        match app.handle_key(key) {
            UiAction::Quit => break,
            UiAction::Command(verb, true) => {
                action_with_args(app, verb, &["--shell"])?;
                if verb == "attach" {
                    break;
                }
            }
            UiAction::Command(verb, false) => {
                action(app, verb)?;
                if verb == "attach" {
                    break;
                }
            }
            UiAction::Reload => {
                refresh_all(app)?;
            }
            UiAction::UpgradeAll => upgrade_all(app)?,
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
            Constraint::Length(5),
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
    f.render_widget(
        Paragraph::new(format!(
            "Filter{}: {}",
            if app.filtering {
                " (typing; Enter keeps, Esc clears)"
            } else {
                " (/ to edit)"
            },
            app.filter
        ))
        .block(Block::default().borders(Borders::ALL)),
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
            "Status: {}\n\
             j/k move • / filter • Esc close • q quit • a Herdr • S shell\n\
             s start • x stop • d diff • i inspect • u selected upgrade • U all upgrades • r refresh",
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
        app.detail = "details".into();
        assert_eq!(app.handle_key(KeyCode::Esc), UiAction::None);
        assert!(app.detail.is_empty());
        assert_eq!(app.handle_key(KeyCode::Char('r')), UiAction::Reload);
        assert_eq!(app.handle_key(KeyCode::Char('q')), UiAction::Quit);
        app.handle_key(KeyCode::Char('/'));
        app.handle_key(KeyCode::Char('z'));
        assert_eq!(app.filter, "z");
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
            "#!/bin/sh\ncase \"$*\" in *diff*) printf '{\"diff\":[]}\\n';; *inspect*) printf '{\"spec\":{\"name\":\"alpha\",\"host\":\"lab\",\"project_path\":\"/tmp\",\"user\":\"gerald\",\"profile\":{\"image\":\"test:latest\"}},\"state\":\"running\",\"drift\":false}\\n';; *upgrade*) printf '[{\"spec\":{\"name\":\"alpha\"},\"state\":\"running\",\"drift\":false}]\\n';; esac\nexit 0\n",
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
        upgrade_all(&mut app).unwrap();
        assert_eq!(app.message, "upgrade all: completed");
        assert_eq!(App::load().unwrap().lanes.len(), 1);
        let mut keys = vec![
            None,
            Some(KeyCode::Backspace),
            Some(KeyCode::Char('s')),
            Some(KeyCode::Char('x')),
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
        assert_eq!(redraws, 6);
        assert_eq!(app.message, "refreshed reachable lanes");
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
        };
        action(&mut empty, "start").unwrap();
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
        assert!(text.contains("Esc close"));
        assert!(text.contains("selected upgrade"));
        assert!(text.contains("all upgrades"));

        let inspected = inspect_detail(
            br#"{"spec":{"name":"alpha","host":"lab","project_path":"/tmp","user":"gerald","profile":{"image":"test:latest"}},"state":"running","drift":false}"#,
        )
        .unwrap();
        assert!(inspected.contains("Home: /home/dev"));
    }
}
