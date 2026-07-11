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
        terminal.draw(|f| {
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
        })?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        if let Event::Key(k) = event::read()? {
            match k.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('j') | KeyCode::Down => {
                    let n = app.visible().len();
                    if n > 0 {
                        app.selected = (app.selected + 1).min(n - 1)
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => app.selected = app.selected.saturating_sub(1),
                KeyCode::Char('s') => action(&mut app, "start")?,
                KeyCode::Char('x') => action(&mut app, "stop")?,
                KeyCode::Char('u') => action(&mut app, "upgrade")?,
                KeyCode::Char('a') => action(&mut app, "attach")?,
                KeyCode::Char('S') => action_with_args(&mut app, "attach", &["--shell"])?,
                KeyCode::Char('i') => action(&mut app, "inspect")?,
                KeyCode::Char('r') => {
                    app.lanes = Store::open_default()?.lanes()?;
                    app.message = "reloaded cached status".into()
                }
                KeyCode::Backspace => {
                    app.filter.pop();
                    app.selected = 0
                }
                KeyCode::Char(c) => {
                    app.filter.push(c);
                    app.selected = 0
                }
                _ => {}
            }
        }
    }
    Ok(())
}
