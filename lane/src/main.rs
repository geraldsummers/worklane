use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};
use std::{io, process::Command, time::Duration};
use worklane_core::{LaneStatus, Store};

struct App {
    lanes: Vec<LaneStatus>,
    selected: usize,
    filter: String,
    message: String,
}
#[derive(Debug, PartialEq, Eq)]
enum UiAction {
    None,
    Quit,
    Command(&'static str, bool),
    Reload,
}
impl App {
    fn load() -> Result<Self> {
        Ok(Self {
            lanes: Store::open_default()?.lanes()?,
            selected: 0,
            filter: String::new(),
            message: "j/k move • a Herdr • S shell • s start • x stop • u upgrade • i inspect • r refresh • q quit"
                .into(),
        })
    }
    fn visible(&self) -> Vec<&LaneStatus> {
        self.lanes
            .iter()
            .filter(|x| x.spec.name.contains(&self.filter) || x.spec.host.contains(&self.filter))
            .collect()
    }
    fn handle_key(&mut self, key: KeyCode) -> UiAction {
        match key {
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
            KeyCode::Char('a') => UiAction::Command("attach", false),
            KeyCode::Char('S') => UiAction::Command("attach", true),
            KeyCode::Char('i') => UiAction::Command("inspect", false),
            KeyCode::Char('r') => UiAction::Reload,
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
        }
    }
}
fn action(app: &mut App, verb: &str) -> Result<()> {
    action_with_args(app, verb, &[])
}
fn action_with_args(app: &mut App, verb: &str, extra: &[&str]) -> Result<()> {
    let visible = app.visible();
    let Some(item) = visible.get(app.selected) else {
        return Ok(());
    };
    let mut args = vec!["lane", verb, &item.spec.id];
    args.extend(extra.iter().copied());
    let status = Command::new("worklane").args(args).status()?;
    app.message = format!("{verb}: {}", if status.success() { "ok" } else { "failed" });
    app.lanes = Store::open_default()?.lanes()?;
    Ok(())
}
fn main() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}
fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = App::load()?;
    loop {
        terminal.draw(|f| draw(f, &app))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        if let Event::Key(k) = event::read()? {
            match app.handle_key(k.code) {
                UiAction::Quit => break,
                UiAction::Command(verb, true) => action_with_args(&mut app, verb, &["--shell"])?,
                UiAction::Command(verb, false) => action(&mut app, verb)?,
                UiAction::Reload => {
                    app.lanes = Store::open_default()?.lanes()?;
                    app.message = "reloaded cached status".into()
                }
                UiAction::None => {}
            }
        }
    }
    Ok(())
}
fn draw(f: &mut ratatui::Frame, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(2),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(f.area());
    let items = app
        .visible()
        .iter()
        .enumerate()
        .map(|(i, x)| {
            let flag = if x.drift { " drift" } else { "" };
            ListItem::new(format!(
                "{}  {:<10} {:<10} {}{}",
                x.spec.name, x.spec.host, x.state, x.spec.profile.image, flag
            ))
            .style(if i == app.selected {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            })
        })
        .collect::<Vec<_>>();
    f.render_widget(
        List::new(items).block(
            Block::default()
                .title(" Worklane lanes ")
                .borders(Borders::ALL),
        ),
        areas[0],
    );
    f.render_widget(
        Paragraph::new(format!("Filter: {}", app.filter))
            .block(Block::default().borders(Borders::ALL)),
        areas[1],
    );
    f.render_widget(
        Paragraph::new(app.message.as_str()).block(Block::default().borders(Borders::ALL)),
        areas[2],
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
        assert_eq!(
            app.handle_key(KeyCode::Char('i')),
            UiAction::Command("inspect", false)
        );
        assert_eq!(app.handle_key(KeyCode::Char('r')), UiAction::Reload);
        assert_eq!(app.handle_key(KeyCode::Char('q')), UiAction::Quit);
        app.handle_key(KeyCode::Char('z'));
        assert_eq!(app.filter, "z");
        app.handle_key(KeyCode::Backspace);
        assert!(app.filter.is_empty());
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected, 0);
        app.handle_key(KeyCode::Up);
        assert_eq!(app.selected, 0);
        assert_eq!(app.handle_key(KeyCode::F(1)), UiAction::None);
    }

    #[test]
    fn actions_reload_the_cached_store_and_run_worklane() {
        let _guard = environment_lock().lock().unwrap();
        let root = env::temp_dir().join(format!("lane-ui-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let worklane = bin.join("worklane");
        fs::write(&worklane, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&worklane, fs::Permissions::from_mode(0o755)).unwrap();
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
        assert_eq!(App::load().unwrap().lanes.len(), 1);
        let mut empty = App {
            lanes: vec![],
            selected: 0,
            filter: String::new(),
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
    }
}
