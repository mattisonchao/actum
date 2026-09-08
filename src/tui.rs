use std::{
    collections::BTreeMap,
    io,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{Local, NaiveDate};
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
    model::{GroupView, GroupWithTasks, Snapshot, TaskCounts, TaskView, format_deadline},
    store::{Store, is_finished_archive, normalize_path},
};

#[derive(Clone, Copy)]
enum Focus {
    Directories,
    Tasks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewMode {
    Today,
    Backlog,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompletedView {
    Directory,
    FinishDate,
}

impl CompletedView {
    fn label(self) -> &'static str {
        match self {
            Self::Directory => "Directory",
            Self::FinishDate => "Finish date",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompletionDate {
    date: Option<NaiveDate>,
    count: usize,
}

impl ViewMode {
    fn previous(self) -> Self {
        match self {
            Self::Today => Self::Completed,
            Self::Backlog => Self::Today,
            Self::Completed => Self::Backlog,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Today => Self::Backlog,
            Self::Backlog => Self::Completed,
            Self::Completed => Self::Today,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Backlog => "Backlog",
            Self::Completed => "Completed",
        }
    }
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
            Self::Group(group) => group
                .tasks
                .iter()
                .filter_map(|task| task.effective_deadline)
                .min()
                .or(group.group.effective_deadline),
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
    counts: TaskCounts,
    today: NaiveDate,
    view: ViewMode,
    completed_view: CompletedView,
    completion_dates: Vec<CompletionDate>,
    directory_index: usize,
    task_index: usize,
    focus: Focus,
    message: String,
}

impl App {
    async fn new(store: &Store, root: String) -> Result<Self> {
        let root = normalize_path(&root);
        let mut app = Self {
            root: root.clone(),
            root_snapshot: empty_snapshot(&root),
            task_snapshot: empty_snapshot(&root),
            counts: TaskCounts::default(),
            today: Local::now().date_naive(),
            view: ViewMode::Today,
            completed_view: CompletedView::Directory,
            completion_dates: Vec::new(),
            directory_index: 0,
            task_index: 0,
            focus: Focus::Directories,
            message: "1/2/3 views · [/] switch · tab focus · enter open · q quit".into(),
        };
        app.refresh(store).await?;
        Ok(app)
    }

    async fn refresh(&mut self, store: &Store) -> Result<()> {
        self.today = Local::now().date_naive();
        self.counts = store.task_counts(&self.root, self.today).await?;
        self.root_snapshot = store.snapshot(&self.root, false).await?;
        filter_directories(&mut self.root_snapshot, self.view, self.today);

        if self.view == ViewMode::Completed && self.completed_view == CompletedView::FinishDate {
            let completed_tasks = store.completed_tasks(&self.root).await?;
            self.completion_dates = completion_dates(&completed_tasks);
            self.directory_index = self
                .directory_index
                .min(self.completion_dates.len().saturating_sub(1));
            let selected_date = self
                .completion_dates
                .get(self.directory_index)
                .map(|entry| entry.date)
                .unwrap_or(None);
            self.task_snapshot =
                completion_date_snapshot(&self.root, completed_tasks, selected_date);
            self.task_index = self
                .task_index
                .min(flatten_tasks(&self.task_snapshot).len().saturating_sub(1));
            return Ok(());
        }

        self.completion_dates.clear();
        self.directory_index = self
            .directory_index
            .min(self.root_snapshot.directories.len().saturating_sub(1));
        let selected_path = self
            .root_snapshot
            .directories
            .get(self.directory_index)
            .map(|directory| directory.path.as_str())
            .unwrap_or(&self.root);
        self.task_snapshot = if self.view == ViewMode::Completed {
            completion_directory_snapshot(
                selected_path,
                store.completed_tasks(selected_path).await?,
            )
        } else {
            let mut snapshot = store.snapshot(selected_path, false).await?;
            filter_tasks(&mut snapshot, self.view, self.today);
            snapshot
        };
        self.task_index = self
            .task_index
            .min(flatten_tasks(&self.task_snapshot).len().saturating_sub(1));
        Ok(())
    }

    async fn set_view(&mut self, store: &Store, view: ViewMode) -> Result<()> {
        if self.view != view {
            self.view = view;
            self.directory_index = 0;
            self.task_index = 0;
        }
        self.refresh(store).await?;
        self.message = format!("{} view", self.view.label());
        Ok(())
    }

    async fn set_completed_view(
        &mut self,
        store: &Store,
        completed_view: CompletedView,
    ) -> Result<()> {
        if self.view == ViewMode::Completed && self.completed_view != completed_view {
            self.completed_view = completed_view;
            self.directory_index = 0;
            self.task_index = 0;
        }
        self.refresh(store).await?;
        self.message = format!("Completed · {} view", self.completed_view.label());
        Ok(())
    }

    fn navigation_len(&self) -> usize {
        if self.view == ViewMode::Completed && self.completed_view == CompletedView::FinishDate {
            self.completion_dates.len()
        } else {
            self.root_snapshot.directories.len()
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::Directories => {
                self.directory_index =
                    shifted_index(self.directory_index, delta, self.navigation_len());
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
                KeyCode::Char('1') => app.set_view(store, ViewMode::Today).await?,
                KeyCode::Char('2') => app.set_view(store, ViewMode::Backlog).await?,
                KeyCode::Char('3') => app.set_view(store, ViewMode::Completed).await?,
                KeyCode::Char('[') => {
                    app.set_view(store, app.view.previous()).await?;
                }
                KeyCode::Char(']') => {
                    app.set_view(store, app.view.next()).await?;
                }
                KeyCode::Char('d') if app.view == ViewMode::Completed => {
                    app.set_completed_view(store, CompletedView::Directory)
                        .await?;
                }
                KeyCode::Char('f') if app.view == ViewMode::Completed => {
                    app.set_completed_view(store, CompletedView::FinishDate)
                        .await?;
                }
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
                        if app.view == ViewMode::Completed
                            && app.completed_view == CompletedView::FinishDate
                        {
                            app.message = "finish date selected".into();
                        } else if let Some(directory) =
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
                        if task.status == "completed" {
                            app.message = format!("task #{} is already completed", task.id);
                        } else {
                            store
                                .complete_task(task.id, Some("completed from sidebar"))
                                .await?;
                            app.message = format!("completed #{}: {}", task.id, task.title);
                            app.refresh(store).await?;
                        }
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
        Constraint::Length(4),
        Constraint::Percentage(55),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .split(frame.area());
    let top_areas = Layout::horizontal([Constraint::Percentage(32), Constraint::Percentage(68)])
        .split(areas[1]);

    let mut header_lines = vec![Line::from(vec![
        Span::styled(
            " actum ",
            Style::default().fg(Color::Black).bg(Color::Green),
        ),
        Span::raw("  "),
        view_tab(
            ViewMode::Today,
            app.counts.today,
            app.view == ViewMode::Today,
        ),
        Span::raw(" "),
        view_tab(
            ViewMode::Backlog,
            app.counts.backlog,
            app.view == ViewMode::Backlog,
        ),
        Span::raw(" "),
        view_tab(
            ViewMode::Completed,
            app.counts.completed,
            app.view == ViewMode::Completed,
        ),
    ])];
    let context_line = if app.view == ViewMode::Completed {
        Line::from(vec![
            Span::raw(" Archive  "),
            completed_view_tab(
                CompletedView::Directory,
                app.completed_view == CompletedView::Directory,
            ),
            Span::raw(" "),
            completed_view_tab(
                CompletedView::FinishDate,
                app.completed_view == CompletedView::FinishDate,
            ),
            Span::raw(format!("  {}", app.task_snapshot.path)),
        ])
    } else {
        Line::from(format!(" {}", app.task_snapshot.path))
    };
    header_lines.push(context_line);
    let header = Paragraph::new(header_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" PostgreSQL connected "),
    );
    frame.render_widget(header, areas[0]);

    let directory_items = navigation_items(app);
    let directory_title =
        if app.view == ViewMode::Completed && app.completed_view == CompletedView::FinishDate {
            format!(" Finish dates · {} ", app.root)
        } else {
            format!(" Directories · {} ", app.root)
        };
    frame.render_widget(
        List::new(directory_items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(directory_title),
        ),
        top_areas[0],
    );

    let selected_task_id = app.selected_task().map(|task| task.id);
    let mut task_items = Vec::new();
    let mut selected_visual_index = 0;
    let mut visual_index = 0;
    let marker_width = if app.view == ViewMode::Completed {
        20
    } else {
        8
    };
    let task_width = usize::from(top_areas[1].width.saturating_sub(marker_width)).max(8);
    let archive_mode = app.view == ViewMode::Completed;
    for section in task_sections(&app.task_snapshot) {
        let heading = match section {
            TaskSection::Group(group) => Line::from(vec![
                Span::styled(
                    format!(" {} ", group.group.name.to_ascii_uppercase()),
                    group_badge_style(group.group.id),
                ),
                Span::styled(
                    format!(
                        "  {} {}",
                        group.tasks.len(),
                        if archive_mode { "finished" } else { "open" }
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            TaskSection::Ungrouped(_) => Line::from(Span::styled(
                "UNGROUPED",
                Style::default().fg(Color::DarkGray),
            )),
        };
        task_items.push(ListItem::new(heading));
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
                app.view,
            ));
            visual_index += 1;
        }
    }
    if task_items.is_empty() {
        task_items.push(ListItem::new(Line::from(Span::styled(
            match app.view {
                ViewMode::Today => "No tasks due today",
                ViewMode::Backlog => "No open tasks",
                ViewMode::Completed => "No completed tasks",
            },
            Style::default().fg(Color::DarkGray),
        ))));
    }
    let mut task_state = ListState::default();
    if selected_task_id.is_some() {
        task_state.select(Some(selected_visual_index));
    }
    let task_title = match app.view {
        ViewMode::Today => " Tasks · due today + overdue ",
        ViewMode::Backlog => " Tasks · all open ",
        ViewMode::Completed => " Tasks · completed archive ",
    };
    frame.render_stateful_widget(
        List::new(task_items).block(Block::default().borders(Borders::ALL).title(task_title)),
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
    view: ViewMode,
) -> ListItem<'static> {
    let title = format!("#{} {}", task.id, task.title);
    let mut wrapped_title = textwrap::wrap(&title, text_width).into_iter();
    let marker = if view == ViewMode::Completed {
        completion_span(task)
    } else {
        deadline_span(task.effective_deadline)
    };
    let mut lines = vec![Line::from(vec![
        marker,
        Span::raw(format!("  {}", wrapped_title.next().unwrap_or_default())),
    ])];
    lines.extend(wrapped_title.map(|line| Line::from(format!("      {line}"))));
    let mut item = ListItem::new(lines);
    if selected {
        item = item.style(selected_style(focused));
    }
    item
}

fn completion_span(task: &TaskView) -> Span<'static> {
    let label = task.completed_at.as_ref().map_or_else(
        || " DONE — ".into(),
        |timestamp| {
            format!(
                " DONE {} ",
                timestamp.with_timezone(&Local).format("%m-%d %H:%M")
            )
        },
    );
    Span::styled(
        label,
        Style::default()
            .fg(Color::Rgb(185, 235, 210))
            .bg(Color::Rgb(30, 79, 62))
            .add_modifier(Modifier::BOLD),
    )
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
            Span::raw(task.directory_path.clone()),
            Span::raw("   "),
            Span::styled("GROUP ", label_style),
            Span::raw(group.to_owned()),
        ]),
    ];
    if let Some(completed_at) = task.completed_at {
        lines.push(Line::from(vec![
            Span::styled("FINISHED ", label_style),
            Span::raw(
                completed_at
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string(),
            ),
        ]));
    }
    lines.extend([
        Line::from(""),
        Line::from(Span::styled("TITLE", label_style)),
        Line::from(task.title.clone()),
        Line::from(""),
        Line::from(Span::styled("LINKS", label_style)),
    ]);

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
        task.completion_note.as_ref().map_or_else(
            || {
                Line::from(Span::styled(
                    "No comments yet",
                    Style::default().fg(Color::DarkGray),
                ))
            },
            |note| Line::from(note.clone()),
        ),
    ]);
    lines
}

fn navigation_items(app: &App) -> Vec<ListItem<'static>> {
    if app.view == ViewMode::Completed && app.completed_view == CompletedView::FinishDate {
        if app.completion_dates.is_empty() {
            return vec![ListItem::new(Line::from(Span::styled(
                "No completion dates",
                Style::default().fg(Color::DarkGray),
            )))];
        }

        return app
            .completion_dates
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let date = entry
                    .date
                    .map(|date| date.to_string())
                    .unwrap_or_else(|| "Unknown date".into());
                let mut item = ListItem::new(Line::from(vec![
                    Span::raw(format!("  {date}")),
                    Span::styled(
                        format!("  {} tasks", entry.count),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                if index == app.directory_index {
                    item = item.style(selected_style(matches!(app.focus, Focus::Directories)));
                }
                item
            })
            .collect();
    }

    if app.root_snapshot.directories.is_empty() {
        return vec![ListItem::new(Line::from(Span::styled(
            match app.view {
                ViewMode::Today => "No directories with tasks due today",
                ViewMode::Backlog => "No backlog directories",
                ViewMode::Completed => "No directories with completed tasks",
            },
            Style::default().fg(Color::DarkGray),
        )))];
    }

    app.root_snapshot
        .directories
        .iter()
        .enumerate()
        .map(|(index, directory)| {
            let line = if app.view == ViewMode::Completed {
                Line::from(vec![
                    Span::raw(format!("  {}/", directory.name)),
                    Span::styled(
                        format!("  {} completed", directory.completed_task_count),
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
            } else {
                Line::from(vec![
                    deadline_span(directory.effective_deadline),
                    Span::raw(format!("  {}/", directory.name)),
                ])
            };
            let mut item = ListItem::new(line);
            if index == app.directory_index {
                item = item.style(selected_style(matches!(app.focus, Focus::Directories)));
            }
            item
        })
        .collect()
}

fn completion_dates(tasks: &[TaskView]) -> Vec<CompletionDate> {
    let mut dated = BTreeMap::<NaiveDate, usize>::new();
    let mut unknown = 0;
    for task in tasks {
        if let Some(date) = task
            .completed_at
            .as_ref()
            .map(|timestamp| timestamp.with_timezone(&Local).date_naive())
        {
            *dated.entry(date).or_default() += 1;
        } else {
            unknown += 1;
        }
    }

    let mut dates = dated
        .into_iter()
        .rev()
        .map(|(date, count)| CompletionDate {
            date: Some(date),
            count,
        })
        .collect::<Vec<_>>();
    if unknown > 0 {
        dates.push(CompletionDate {
            date: None,
            count: unknown,
        });
    }
    dates
}

fn completion_directory_snapshot(path: &str, tasks: Vec<TaskView>) -> Snapshot {
    let mut grouped = BTreeMap::<(String, i64), Vec<TaskView>>::new();
    let mut ungrouped_tasks = Vec::new();
    for task in tasks {
        match (task.group_name.clone(), task.group_id) {
            (Some(group_name), Some(group_id)) => {
                grouped
                    .entry((group_name, group_id))
                    .or_default()
                    .push(task);
            }
            _ => ungrouped_tasks.push(task),
        }
    }

    let groups = grouped
        .into_iter()
        .map(|((name, id), tasks)| {
            let effective_deadline = tasks
                .iter()
                .filter_map(|task| task.effective_deadline)
                .min();
            GroupWithTasks {
                group: GroupView {
                    id,
                    name,
                    explicit_deadline: None,
                    effective_deadline,
                    links: Vec::new(),
                },
                tasks,
            }
        })
        .collect();

    Snapshot {
        directory_id: None,
        path: format!("{path} · completed"),
        directories: Vec::new(),
        groups,
        ungrouped_tasks,
    }
}

fn completion_date_snapshot(
    root: &str,
    tasks: Vec<TaskView>,
    selected_date: Option<NaiveDate>,
) -> Snapshot {
    let mut projects = BTreeMap::<String, Vec<TaskView>>::new();
    for task in tasks {
        let task_date = task
            .completed_at
            .as_ref()
            .map(|timestamp| timestamp.with_timezone(&Local).date_naive());
        if task_date == selected_date {
            projects
                .entry(project_name(&task.directory_path))
                .or_default()
                .push(task);
        }
    }

    let groups = projects
        .into_iter()
        .map(|(project, tasks)| {
            let id = tasks
                .first()
                .map(|task| task.directory_id)
                .unwrap_or_default();
            GroupWithTasks {
                group: GroupView {
                    id,
                    name: project,
                    explicit_deadline: None,
                    effective_deadline: None,
                    links: Vec::new(),
                },
                tasks,
            }
        })
        .collect();

    let date_label = selected_date
        .map(|date| date.to_string())
        .unwrap_or_else(|| "unknown date".into());
    Snapshot {
        directory_id: None,
        path: format!("{root} · finished {date_label}"),
        directories: Vec::new(),
        groups,
        ungrouped_tasks: Vec::new(),
    }
}

fn project_name(path: &str) -> String {
    let components = path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    if components.first() == Some(&"projects") && components.len() > 1 {
        components[1].to_owned()
    } else {
        components
            .iter()
            .position(|component| component.eq_ignore_ascii_case("finished"))
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| components.get(index))
            .or_else(|| components.last())
            .copied()
            .unwrap_or("root")
            .to_owned()
    }
}

fn empty_snapshot(path: &str) -> Snapshot {
    Snapshot {
        directory_id: None,
        path: path.to_owned(),
        directories: Vec::new(),
        groups: Vec::new(),
        ungrouped_tasks: Vec::new(),
    }
}

fn filter_directories(snapshot: &mut Snapshot, view: ViewMode, today: NaiveDate) {
    snapshot.directories.retain(|directory| match view {
        ViewMode::Today => {
            !is_finished_archive(&directory.path)
                && directory
                    .effective_deadline
                    .is_some_and(|deadline| deadline <= today)
        }
        ViewMode::Backlog => !is_finished_archive(&directory.path),
        ViewMode::Completed => directory.completed_task_count > 0,
    });
}

fn filter_tasks(snapshot: &mut Snapshot, view: ViewMode, today: NaiveDate) {
    let keep = |task: &TaskView| match view {
        ViewMode::Today => {
            task.status == "open"
                && task
                    .effective_deadline
                    .is_some_and(|deadline| deadline <= today)
        }
        ViewMode::Backlog => task.status == "open",
        ViewMode::Completed => task.status == "completed",
    };

    for group in &mut snapshot.groups {
        group.tasks.retain(&keep);
    }
    snapshot.groups.retain(|group| !group.tasks.is_empty());
    snapshot.ungrouped_tasks.retain(keep);
}

fn view_tab(view: ViewMode, count: i64, selected: bool) -> Span<'static> {
    let shortcut = match view {
        ViewMode::Today => 1,
        ViewMode::Backlog => 2,
        ViewMode::Completed => 3,
    };
    let style = if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(110, 200, 170))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray).bg(Color::Rgb(38, 45, 52))
    };
    Span::styled(format!(" {shortcut} {} ({count}) ", view.label()), style)
}

fn completed_view_tab(view: CompletedView, selected: bool) -> Span<'static> {
    let shortcut = match view {
        CompletedView::Directory => 'd',
        CompletedView::FinishDate => 'f',
    };
    let style = if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(198, 157, 87))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray).bg(Color::Rgb(38, 45, 52))
    };
    Span::styled(format!(" {shortcut} {} ", view.label()), style)
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

fn group_badge_style(group_id: i64) -> Style {
    const BACKGROUNDS: [Color; 8] = [
        Color::Rgb(48, 78, 160),
        Color::Rgb(104, 63, 151),
        Color::Rgb(20, 108, 105),
        Color::Rgb(145, 79, 24),
        Color::Rgb(132, 52, 90),
        Color::Rgb(49, 105, 65),
        Color::Rgb(64, 73, 140),
        Color::Rgb(132, 61, 48),
    ];
    let palette_index = group_id.rem_euclid(BACKGROUNDS.len() as i64) as usize;
    Style::default()
        .fg(Color::White)
        .bg(BACKGROUNDS[palette_index])
        .add_modifier(Modifier::BOLD)
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
    use chrono::{DateTime, NaiveDate, Utc};

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
                directory_path: "/projects/example".into(),
                group_id: None,
                group_name: None,
                title: "Ship the release".into(),
                explicit_deadline: NaiveDate::from_ymd_opt(2030, 6, 1),
                effective_deadline: NaiveDate::from_ymd_opt(2030, 6, 1),
                status: "completed".into(),
                completed_at: Some(
                    DateTime::parse_from_rfc3339("2030-06-01T09:30:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                ),
                completion_note: Some("Verified in production".into()),
                links: vec!["https://example.com/task/42".into()],
            }],
        };
        let app = App {
            root: "/projects".into(),
            root_snapshot: empty_snapshot,
            task_snapshot,
            counts: TaskCounts::default(),
            today: NaiveDate::from_ymd_opt(2030, 6, 1).unwrap(),
            view: ViewMode::Backlog,
            completed_view: CompletedView::Directory,
            completion_dates: Vec::new(),
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
        assert!(rendered.contains("FINISHED"));
        assert!(rendered.contains("COMMENTS & NOTES"));
        assert!(rendered.contains("Verified in production"));
    }

    #[test]
    fn dated_sections_sort_before_undated_sections() {
        let dated_task = TaskView {
            id: 2,
            directory_id: 2,
            directory_path: "/projects/example".into(),
            group_id: None,
            group_name: None,
            title: "Dated task".into(),
            explicit_deadline: NaiveDate::from_ymd_opt(2030, 6, 1),
            effective_deadline: NaiveDate::from_ymd_opt(2030, 6, 1),
            status: "open".into(),
            completed_at: None,
            completion_note: None,
            links: Vec::new(),
        };
        let undated_task = TaskView {
            id: 1,
            directory_id: 2,
            directory_path: "/projects/example".into(),
            group_id: Some(3),
            group_name: Some("Later".into()),
            title: "Undated task".into(),
            explicit_deadline: None,
            effective_deadline: None,
            status: "open".into(),
            completed_at: None,
            completion_note: None,
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

    #[test]
    fn group_badges_have_stable_colored_backgrounds() {
        let first = group_badge_style(7);
        assert!(first.bg.is_some());
        assert_eq!(first, group_badge_style(7));
        assert_ne!(first, group_badge_style(8));
    }

    #[test]
    fn task_views_filter_today_backlog_and_completed() {
        let today = NaiveDate::from_ymd_opt(2030, 6, 1).unwrap();
        let due = TaskView {
            id: 1,
            directory_id: 2,
            directory_path: "/projects/example".into(),
            group_id: None,
            group_name: None,
            title: "Due".into(),
            explicit_deadline: Some(today),
            effective_deadline: Some(today),
            status: "open".into(),
            completed_at: None,
            completion_note: None,
            links: Vec::new(),
        };
        let undated = TaskView {
            id: 2,
            title: "Undated".into(),
            explicit_deadline: None,
            effective_deadline: None,
            ..due.clone()
        };
        let completed = TaskView {
            id: 3,
            title: "Completed".into(),
            status: "completed".into(),
            ..due.clone()
        };
        let snapshot = Snapshot {
            directory_id: Some(2),
            path: "/projects/example".into(),
            directories: Vec::new(),
            groups: Vec::new(),
            ungrouped_tasks: vec![due, undated, completed],
        };

        let mut today_snapshot = snapshot.clone();
        filter_tasks(&mut today_snapshot, ViewMode::Today, today);
        assert_eq!(today_snapshot.ungrouped_tasks.len(), 1);
        assert_eq!(today_snapshot.ungrouped_tasks[0].id, 1);

        let mut backlog_snapshot = snapshot.clone();
        filter_tasks(&mut backlog_snapshot, ViewMode::Backlog, today);
        assert_eq!(backlog_snapshot.ungrouped_tasks.len(), 2);

        let mut completed_snapshot = snapshot;
        filter_tasks(&mut completed_snapshot, ViewMode::Completed, today);
        assert_eq!(completed_snapshot.ungrouped_tasks.len(), 1);
        assert_eq!(completed_snapshot.ungrouped_tasks[0].id, 3);
    }

    #[test]
    fn completed_tasks_group_by_finish_date_and_project() {
        let timestamp = DateTime::parse_from_rfc3339("2030-06-01T09:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let task = TaskView {
            id: 7,
            directory_id: 16,
            directory_path: "/projects/oxia/finished".into(),
            group_id: Some(4),
            group_name: Some("Subscriptions".into()),
            title: "Ship".into(),
            explicit_deadline: None,
            effective_deadline: None,
            status: "completed".into(),
            completed_at: Some(timestamp),
            completion_note: Some("Verified".into()),
            links: vec!["https://example.com/7".into()],
        };
        let expected_date = timestamp.with_timezone(&Local).date_naive();

        assert_eq!(
            completion_dates(std::slice::from_ref(&task)),
            vec![CompletionDate {
                date: Some(expected_date),
                count: 1,
            }]
        );
        let snapshot = completion_date_snapshot("/projects", vec![task], Some(expected_date));
        assert_eq!(snapshot.groups.len(), 1);
        assert_eq!(snapshot.groups[0].group.name, "oxia");
        assert_eq!(
            snapshot.groups[0].tasks[0].completion_note.as_deref(),
            Some("Verified")
        );
    }
}
