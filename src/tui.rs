use std::{
    io,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::{
    model::{Snapshot, TaskView, format_priority},
    store::{Store, normalize_path},
};

#[derive(Clone, Copy)]
enum Focus {
    Directories,
    Tasks,
}

struct App {
    root: String,
    root_snapshot: Snapshot,
    task_snapshot: Snapshot,
    directory_index: usize,
    task_index: usize,
    focus: Focus,
    message: String,
}

impl App {
    async fn new(store: &Store, root: String) -> Result<Self> {
        let root = normalize_path(&root);
        let root_snapshot = store.snapshot(&root, false).await?;
        let task_path = root_snapshot
            .directories
            .first()
            .map(|directory| directory.path.as_str())
            .unwrap_or(&root);
        let task_snapshot = store.snapshot(task_path, false).await?;
        Ok(Self {
            root,
            root_snapshot,
            task_snapshot,
            directory_index: 0,
            task_index: 0,
            focus: Focus::Directories,
            message: "q quit · tab focus · enter open · space complete".into(),
        })
    }

    async fn refresh(&mut self, store: &Store) -> Result<()> {
        self.root_snapshot = store.snapshot(&self.root, false).await?;
        self.directory_index = self
            .directory_index
            .min(self.root_snapshot.directories.len().saturating_sub(1));
        let task_path = self
            .root_snapshot
            .directories
            .get(self.directory_index)
            .map(|directory| directory.path.as_str())
            .unwrap_or(&self.root);
        self.task_snapshot = store.snapshot(task_path, false).await?;
        self.task_index = self
            .task_index
            .min(flatten_tasks(&self.task_snapshot).len().saturating_sub(1));
        Ok(())
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::Directories => {
                self.directory_index = shifted_index(
                    self.directory_index,
                    delta,
                    self.root_snapshot.directories.len(),
                );
                self.task_index = 0;
            }
            Focus::Tasks => {
                self.task_index = shifted_index(
                    self.task_index,
                    delta,
                    flatten_tasks(&self.task_snapshot).len(),
                );
            }
        }
    }

    fn selected_task(&self) -> Option<&TaskView> {
        flatten_tasks(&self.task_snapshot)
            .get(self.task_index)
            .copied()
    }
}

pub async fn run(store: Store, root: String) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &store, root).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    store: &Store,
    root: String,
) -> Result<()> {
    let mut app = App::new(store, root).await?;
    let mut last_refresh = Instant::now();

    loop {
        terminal.draw(|frame| draw(frame, &app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Tab => {
                    app.focus = match app.focus {
                        Focus::Directories => Focus::Tasks,
                        Focus::Tasks => Focus::Directories,
                    };
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.move_selection(-1);
                    if matches!(app.focus, Focus::Directories) {
                        app.refresh(store).await?;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.move_selection(1);
                    if matches!(app.focus, Focus::Directories) {
                        app.refresh(store).await?;
                    }
                }
                KeyCode::Enter => match app.focus {
                    Focus::Directories => {
                        if let Some(directory) =
                            app.root_snapshot.directories.get(app.directory_index)
                        {
                            app.root = directory.path.clone();
                            app.directory_index = 0;
                            app.task_index = 0;
                            app.refresh(store).await?;
                        }
                    }
                    Focus::Tasks => {
                        if let Some(link) = app
                            .selected_task()
                            .and_then(|task| task.links.first())
                            .cloned()
                        {
                            open_link(&link)?;
                            app.message = format!("opened {link}");
                        } else {
                            app.message = "selected task has no link".into();
                        }
                    }
                },
                KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                    app.root = parent_path(&app.root);
                    app.directory_index = 0;
                    app.task_index = 0;
                    app.refresh(store).await?;
                }
                KeyCode::Char(' ') if matches!(app.focus, Focus::Tasks) => {
                    if let Some(task) = app.selected_task().cloned() {
                        store
                            .complete_task(task.id, Some("completed from sidebar"))
                            .await?;
                        app.message = format!("completed #{}: {}", task.id, task.title);
                        app.refresh(store).await?;
                    }
                }
                KeyCode::Char('r') => {
                    app.refresh(store).await?;
                    app.message = "refreshed".into();
                }
                _ => {}
            }
        }

        if last_refresh.elapsed() >= Duration::from_secs(1) {
            app.refresh(store).await?;
            last_refresh = Instant::now();
        }
    }
    Ok(())
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &App) {
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Percentage(35),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .split(frame.area());

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " actum ",
            Style::default().fg(Color::Black).bg(Color::Green),
        ),
        Span::raw(format!("  {}", app.task_snapshot.path)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" PostgreSQL connected "),
    );
    frame.render_widget(header, areas[0]);

    let directory_items = if app.root_snapshot.directories.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No child directories",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        app.root_snapshot
            .directories
            .iter()
            .enumerate()
            .map(|(index, directory)| {
                let line = Line::from(vec![
                    priority_span(directory.effective_priority),
                    Span::raw(format!("  {}/", directory.name)),
                ]);
                let mut item = ListItem::new(line);
                if index == app.directory_index {
                    item = item.style(selected_style(matches!(app.focus, Focus::Directories)));
                }
                item
            })
            .collect()
    };
    frame.render_widget(
        List::new(directory_items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Directories · {} ", app.root)),
        ),
        areas[1],
    );

    let selected_task_id = app.selected_task().map(|task| task.id);
    let mut task_items = Vec::new();
    let mut selected_visual_index = 0;
    let mut visual_index = 0;
    let task_width = usize::from(areas[2].width.saturating_sub(8)).max(8);
    for group in &app.task_snapshot.groups {
        task_items.push(ListItem::new(Line::from(Span::styled(
            format!(
                "{}  {} open",
                group.group.name.to_ascii_uppercase(),
                group.tasks.len()
            ),
            Style::default().fg(Color::DarkGray),
        ))));
        visual_index += 1;
        for task in &group.tasks {
            if selected_task_id == Some(task.id) {
                selected_visual_index = visual_index;
            }
            task_items.push(task_item(
                task,
                selected_task_id == Some(task.id),
                matches!(app.focus, Focus::Tasks),
                task_width,
            ));
            visual_index += 1;
        }
    }
    if !app.task_snapshot.ungrouped_tasks.is_empty() {
        task_items.push(ListItem::new(Line::from(Span::styled(
            "UNGROUPED",
            Style::default().fg(Color::DarkGray),
        ))));
        visual_index += 1;
        for task in &app.task_snapshot.ungrouped_tasks {
            if selected_task_id == Some(task.id) {
                selected_visual_index = visual_index;
            }
            task_items.push(task_item(
                task,
                selected_task_id == Some(task.id),
                matches!(app.focus, Focus::Tasks),
                task_width,
            ));
            visual_index += 1;
        }
    }
    if task_items.is_empty() {
        task_items.push(ListItem::new(Line::from(Span::styled(
            "No pending tasks",
            Style::default().fg(Color::DarkGray),
        ))));
    }
    let mut task_state = ListState::default();
    if selected_task_id.is_some() {
        task_state.select(Some(selected_visual_index));
    }
    frame.render_stateful_widget(
        List::new(task_items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Tasks · completed hidden "),
        ),
        areas[2],
        &mut task_state,
    );
    frame.render_widget(
        Paragraph::new(app.message.as_str()).style(Style::default().fg(Color::DarkGray)),
        areas[3],
    );
}

fn task_item(
    task: &TaskView,
    selected: bool,
    focused: bool,
    text_width: usize,
) -> ListItem<'static> {
    let title = format!("#{} {}", task.id, task.title);
    let mut wrapped_title = textwrap::wrap(&title, text_width).into_iter();
    let mut lines = vec![Line::from(vec![
        priority_span(task.effective_priority),
        Span::raw(format!("  {}", wrapped_title.next().unwrap_or_default())),
    ])];
    lines.extend(wrapped_title.map(|line| Line::from(format!("      {line}"))));
    if selected {
        for link in &task.links {
            lines.extend(textwrap::wrap(link, text_width).into_iter().map(|line| {
                Line::from(Span::styled(
                    format!("      {line}"),
                    Style::default().fg(Color::Blue),
                ))
            }));
        }
    }
    let mut item = ListItem::new(lines);
    if selected {
        item = item.style(selected_style(focused));
    }
    item
}

fn priority_span(priority: i16) -> Span<'static> {
    let style = match priority {
        1 => Style::default()
            .fg(Color::Rgb(255, 180, 173))
            .bg(Color::Rgb(94, 35, 35)),
        2 => Style::default()
            .fg(Color::Rgb(255, 215, 130))
            .bg(Color::Rgb(89, 67, 21)),
        _ => Style::default()
            .fg(Color::Rgb(189, 218, 240))
            .bg(Color::Rgb(41, 63, 83)),
    }
    .add_modifier(Modifier::BOLD);
    Span::styled(format!(" {} ", format_priority(priority)), style)
}

fn selected_style(focused: bool) -> Style {
    if focused {
        Style::default().bg(Color::Rgb(23, 52, 46))
    } else {
        Style::default().bg(Color::Rgb(32, 40, 47))
    }
}

fn flatten_tasks(snapshot: &Snapshot) -> Vec<&TaskView> {
    snapshot
        .groups
        .iter()
        .flat_map(|group| group.tasks.iter())
        .chain(snapshot.ungrouped_tasks.iter())
        .collect()
}

fn shifted_index(current: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    current.saturating_add_signed(delta).min(len - 1)
}

fn parent_path(path: &str) -> String {
    let mut components = path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    components.pop();
    if components.is_empty() {
        "/".into()
    } else {
        format!("/{}", components.join("/"))
    }
}

fn open_link(link: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    Command::new("open").arg(link).spawn()?;
    #[cfg(target_os = "linux")]
    Command::new("xdg-open").arg(link).spawn()?;
    #[cfg(target_os = "windows")]
    Command::new("cmd").args(["/C", "start", link]).spawn()?;
    Ok(())
}
