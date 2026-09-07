mod mcp;
mod model;
mod store;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

use model::{Snapshot, format_deadline, parse_deadline};
use store::Store;

const DEFAULT_DATABASE_URL: &str = "postgres://actum:actum@127.0.0.1:55432/actum";

#[derive(Parser)]
#[command(name = "actum", version, about)]
struct Cli {
    #[arg(long, env = "DATABASE_URL", default_value = DEFAULT_DATABASE_URL, global = true)]
    database_url: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Apply database migrations.
    Migrate,
    /// Ensure the default workspace root exists.
    #[command(visible_alias = "seed")]
    Init,
    /// Print a directory's work with raw links.
    List {
        #[arg(default_value = "/projects")]
        directory: String,
        #[arg(long)]
        all: bool,
    },
    /// Create and update directories.
    Directory {
        #[command(subcommand)]
        command: DirectoryCommand,
    },
    /// Create and update groups.
    Group {
        #[command(subcommand)]
        command: GroupCommand,
    },
    /// Create and update tasks.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Open the database sidebar in the current terminal pane.
    Sidebar {
        #[arg(long, env = "ACTUM_ROOT", default_value = "/projects")]
        root: String,
    },
    /// Run the Actum MCP server over stdio.
    Mcp,
}

#[derive(Subcommand)]
enum DirectoryCommand {
    Add {
        path: String,
        #[arg(long = "deadline", visible_alias = "ddl", value_name = "YYYY-MM-DD")]
        deadline: Option<String>,
    },
}

#[derive(Subcommand)]
enum GroupCommand {
    Add {
        directory: String,
        name: String,
        #[arg(long = "deadline", visible_alias = "ddl", value_name = "YYYY-MM-DD")]
        deadline: Option<String>,
        #[arg(long = "link")]
        links: Vec<String>,
    },
}

#[derive(Subcommand)]
enum TaskCommand {
    Add {
        directory: String,
        title: String,
        #[arg(long)]
        group: Option<String>,
        #[arg(long = "deadline", visible_alias = "ddl", value_name = "YYYY-MM-DD")]
        deadline: Option<String>,
        #[arg(long = "link")]
        links: Vec<String>,
    },
    Complete {
        id: i64,
        #[arg(long)]
        note: Option<String>,
    },
    /// Permanently delete a task by stable ID.
    Delete { id: i64 },
    Move {
        id: i64,
        directory: String,
        #[arg(long)]
        group: Option<String>,
    },
    /// Set a task's deadline by stable ID.
    Deadline {
        id: i64,
        #[arg(value_name = "YYYY-MM-DD")]
        deadline: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    let store = Store::connect(&cli.database_url).await?;
    store.migrate().await?;

    match cli.command {
        None => {
            let root = std::env::var("ACTUM_ROOT").unwrap_or_else(|_| "/projects".to_owned());
            tui::run(store, root).await?;
        }
        Some(Command::Migrate) => println!("Database is ready."),
        Some(Command::Init) => {
            store.initialize().await?;
            println!("Initialized the Actum workspace.");
        }
        Some(Command::List { directory, all }) => {
            print_snapshot(&store.snapshot(&directory, all).await?);
        }
        Some(Command::Directory { command }) => match command {
            DirectoryCommand::Add { path, deadline } => {
                let deadline = parse_deadline(deadline.as_deref())?;
                let id = store.ensure_directory(&path, deadline).await?;
                println!("Created or updated directory #{id}: {path}");
            }
        },
        Some(Command::Group { command }) => match command {
            GroupCommand::Add {
                directory,
                name,
                deadline,
                links,
            } => {
                let deadline = parse_deadline(deadline.as_deref())?;
                let group = store
                    .create_group(&directory, &name, deadline, &links)
                    .await?;
                println!(
                    "{} {}",
                    format_deadline(group.effective_deadline),
                    group.name
                );
                print_links(&group.links, "  ");
            }
        },
        Some(Command::Task { command }) => match command {
            TaskCommand::Add {
                directory,
                title,
                group,
                deadline,
                links,
            } => {
                let deadline = parse_deadline(deadline.as_deref())?;
                let task = store
                    .create_task(&directory, group.as_deref(), &title, deadline, &links)
                    .await?;
                println!(
                    "#{} {} {}",
                    task.id,
                    format_deadline(task.effective_deadline),
                    task.title
                );
                print_links(&task.links, "  ");
            }
            TaskCommand::Complete { id, note } => {
                let task = store.complete_task(id, note.as_deref()).await?;
                println!("Completed #{}: {}", task.id, task.title);
                print_links(&task.links, "  ");
            }
            TaskCommand::Delete { id } => {
                let task = store.delete_task(id).await?;
                println!("Deleted #{}: {}", task.id, task.title);
                print_links(&task.links, "  ");
            }
            TaskCommand::Move {
                id,
                directory,
                group,
            } => {
                let task = store.move_task(id, &directory, group.as_deref()).await?;
                println!("Moved #{} to {directory}: {}", task.id, task.title);
                print_links(&task.links, "  ");
            }
            TaskCommand::Deadline { id, deadline } => {
                let deadline = parse_deadline(Some(&deadline))?
                    .expect("a required deadline always parses to a date");
                let task = store.set_task_deadline(id, deadline).await?;
                println!("Set #{} DDL {}: {}", task.id, deadline, task.title);
            }
        },
        Some(Command::Sidebar { root }) => tui::run(store, root).await?,
        Some(Command::Mcp) => mcp::run(store).await?,
    }
    Ok(())
}

fn print_snapshot(snapshot: &Snapshot) {
    println!("{}", snapshot.path);
    for directory in &snapshot.directories {
        println!(
            "  {} {}/",
            format_deadline(directory.effective_deadline),
            directory.name
        );
    }
    for group in &snapshot.groups {
        println!(
            "\n  {} {} ({})",
            format_deadline(group.group.effective_deadline),
            group.group.name,
            group.tasks.len()
        );
        print_links(&group.group.links, "    ");
        for task in &group.tasks {
            let marker = if task.status == "completed" { "x" } else { " " };
            println!(
                "    [{marker}] #{} {} {}",
                task.id,
                format_deadline(task.effective_deadline),
                task.title
            );
            print_links(&task.links, "        ");
            print_completion_note(task.completion_note.as_deref(), "        ");
        }
    }
    if !snapshot.ungrouped_tasks.is_empty() {
        println!("\n  Ungrouped");
        for task in &snapshot.ungrouped_tasks {
            let marker = if task.status == "completed" { "x" } else { " " };
            println!(
                "    [{marker}] #{} {} {}",
                task.id,
                format_deadline(task.effective_deadline),
                task.title
            );
            print_links(&task.links, "        ");
            print_completion_note(task.completion_note.as_deref(), "        ");
        }
    }
}

fn print_links(links: &[String], indentation: &str) {
    for link in links {
        println!("{indentation}{link}");
    }
}

fn print_completion_note(note: Option<&str>, indentation: &str) {
    if let Some(note) = note {
        println!("{indentation}note: {note}");
    }
}
