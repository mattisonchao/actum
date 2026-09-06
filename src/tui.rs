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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    model::{GroupWithTasks, Snapshot, TaskView, format_deadline},
    store::{Store, normalize_path},
};

#[derive(Clone, Copy)]
enum Focus {
    Directories,
    Tasks,
}

#[derive(Clone, Copy)]
enum TaskSection<'a> {
    Group(&'a GroupWithTasks),
    Ungrouped(&'a [TaskView]),
}

impl<'a> TaskSection<'a> {
    fn tasks(self) -> &'a [TaskView] {
        match self {
            Self::Group(group) => &group.tasks,
            Self::Ungrouped(tasks) => tasks,
        }
    }

    fn deadline(self) -> Option<chrono::NaiveDate> {
        match self {
            Self::Group(group) => group.group.effective_deadline,
            Self::Ungrouped(tasks) => tasks
                .iter()
                .filter_map(|task| task.effective_deadline)
                .min(),
        }
    }
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
        Constraint::Percentage(55),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .split(frame.area());
    let top_areas = Layout::horizontal([Constraint::Percentage(32), Constraint::Percentage(68)])
        .split(areas[1]);

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
                    deadline_span(directory.effective_deadline),
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
        top_areas[0],
    );

    let selected_task_id = app.selected_task().map(|task| task.id);
    let mut task_items = Vec::new();
    let mut selected_visual_index = 0;
    let mut visual_index = 0;
    let task_width = usize::from(top_areas[1].width.saturating_sub(8)).max(8);
    for section in task_sections(&app.task_snapshot) {
        let heading = match section {
            TaskSection::Group(group) => format!(
                "{}  {} open",
                group.group.name.to_ascii_uppercase(),
                group.tasks.len()
            ),
            TaskSection::Ungrouped(_) => "UNGROUPED".into(),
        };
        task_items.push(ListItem::new(Line::from(Span::styled(
            heading,
            Style::default().fg(Color::DarkGray),
        ))));
        visual_index += 1;
        for task in section.tasks() {
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
        top_areas[1],
        &mut task_state,
    );

    let detail_title = app
        .selected_task()
        .map(|task| format!(" Details · Task #{} ", task.id))
        .unwrap_or_else(|| " Details ".into());
    frame.render_widget(
        Paragraph::new(detail_lines(app))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(detail_title)),
        areas[2],
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
        deadline_span(task.effective_deadline),
        Span::raw(format!("  {}", wrapped_title.next().unwrap_or_default())),
    ])];
    lines.extend(wrapped_title.map(|line| Line::from(format!("      {line}"))));
    let mut item = ListItem::new(lines);
    if selected {
        item = item.style(selected_style(focused));
    }
    item
}

fn detail_lines(app: &App) -> Vec<Line<'static>> {
    let Some(task) = app.selected_task() else {
        return vec![Line::from(Span::styled(
            "Select a task to see its ID, details, links, and comments.",
            Style::default().fg(Color::DarkGray),
        ))];
    };

    let label_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);
    let group = task.group_name.as_deref().unwrap_or("Ungrouped");
    let mut lines = vec![
        Line::from(vec![
            Span::styled("ID ", label_style),
            Span::styled(
                format!("#{}", task.id),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("STATUS ", label_style),
            Span::raw(task.status.to_ascii_uppercase()),
            Span::raw("   "),
            Span::styled("DEADLINE ", label_style),
            deadline_span(task.effective_deadline),
        ]),
        Line::from(vec![
            Span::styled("DIRECTORY ", label_style),
            Span::raw(app.task_snapshot.path.clone()),
            Span::raw("   "),
            Span::styled("GROUP ", label_style),
            Span::raw(group.to_owned()),
        ]),
        Line::from(""),
        Line::from(Span::styled("TITLE", label_style)),
        Line::from(task.title.clone()),
        Line::from(""),
        Line::from(Span::styled("LINKS", label_style)),
    ];

    if task.links.is_empty() {
        lines.push(Line::from(Span::styled(
            "No links",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.extend(task.links.iter().map(|link| {
            Line::from(Span::styled(
                format!("• {link}"),
                Style::default().fg(Color::Blue),
            ))
        }));
    }

    lines.extend([
        Line::from(""),
        Line::from(Span::styled("COMMENTS & NOTES", label_style)),
        Line::from(Span::styled(
            "No comments yet",
            Style::default().fg(Color::DarkGray),
        )),
    ]);
    lines
}

fn deadline_span(deadline: Option<chrono::NaiveDate>) -> Span<'static> {
    let style = if deadline.is_some() {
        Style::default()
            .fg(Color::Rgb(255, 215, 130))
            .bg(Color::Rgb(89, 67, 21))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Span::styled(format!(" {} ", format_deadline(deadline)), style)
}

fn selected_style(focused: bool) -> Style {
    if focused {
        Style::default().bg(Color::Rgb(23, 52, 46))
    } else {
        Style::default().bg(Color::Rgb(32, 40, 47))
    }
}

fn flatten_tasks(snapshot: &Snapshot) -> Vec<&TaskView> {
    task_sections(snapshot)
        .into_iter()
        .flat_map(|section| section.tasks().iter())
        .collect()
}

fn task_sections(snapshot: &Snapshot) -> Vec<TaskSection<'_>> {
    let mut sections = snapshot
        .groups
        .iter()
        .map(TaskSection::Group)
        .collect::<Vec<_>>();
    if !snapshot.ungrouped_tasks.is_empty() {
        sections.push(TaskSection::Ungrouped(&snapshot.ungrouped_tasks));
    }
    sections.sort_by_key(|section| {
        let deadline = section.deadline();
        (deadline.is_none(), deadline)
    });
    sections
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

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    #[test]
    fn details_show_the_stable_task_id_and_deadline() {
        let empty_snapshot = Snapshot {
            directory_id: Some(1),
            path: "/projects".into(),
            directories: Vec::new(),
            groups: Vec::new(),
            ungrouped_tasks: Vec::new(),
        };
        let task_snapshot = Snapshot {
            directory_id: Some(2),
            path: "/projects/example".into(),
            directories: Vec::new(),
            groups: Vec::new(),
            ungrouped_tasks: vec![TaskView {
                id: 42,
                directory_id: 2,
                group_id: None,
                group_name: None,
                title: "Ship the release".into(),
                explicit_deadline: NaiveDate::from_ymd_opt(2030, 6, 1),
                effective_deadline: NaiveDate::from_ymd_opt(2030, 6, 1),
                status: "open".into(),
                links: vec!["https://example.com/task/42".into()],
            }],
        };
        let app = App {
            root: "/projects".into(),
            root_snapshot: empty_snapshot,
            task_snapshot,
            directory_index: 0,
            task_index: 0,
            focus: Focus::Tasks,
            message: String::new(),
        };

        let mut rendered = String::new();
        for line in detail_lines(&app) {
            for span in line.spans {
                rendered.push_str(span.content.as_ref());
            }
            rendered.push('\n');
        }

        assert!(rendered.contains("#42"));
        assert!(rendered.contains("DDL 2030-06-01"));
        assert!(rendered.contains("/projects/example"));
        assert!(rendered.contains("COMMENTS & NOTES"));
    }

    #[test]
    fn dated_sections_sort_before_undated_sections() {
        let dated_task = TaskView {
            id: 2,
            directory_id: 2,
            group_id: None,
            group_name: None,
            title: "Dated task".into(),
            explicit_deadline: NaiveDate::from_ymd_opt(2030, 6, 1),
            effective_deadline: NaiveDate::from_ymd_opt(2030, 6, 1),
            status: "open".into(),
            links: Vec::new(),
        };
        let undated_task = TaskView {
            id: 1,
            directory_id: 2,
            group_id: Some(3),
            group_name: Some("Later".into()),
            title: "Undated task".into(),
            explicit_deadline: None,
            effective_deadline: None,
            status: "open".into(),
            links: Vec::new(),
        };
        let snapshot = Snapshot {
            directory_id: Some(2),
            path: "/projects/example".into(),
            directories: Vec::new(),
            groups: vec![GroupWithTasks {
                group: crate::model::GroupView {
                    id: 3,
                    name: "Later".into(),
                    explicit_deadline: None,
                    effective_deadline: None,
                    links: Vec::new(),
                },
                tasks: vec![undated_task],
            }],
            ungrouped_tasks: vec![dated_task],
        };

        let ids = flatten_tasks(&snapshot)
            .into_iter()
            .map(|task| task.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![2, 1]);
    }
}
