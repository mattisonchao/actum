mod mcp;
mod model;
mod store;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

use model::{Snapshot, format_priority, parse_priority};
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
    /// Load an idempotent sample workspace based on the current task list.
    Seed,
    /// Print a directory's pending work with raw links.
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
        #[arg(long)]
        priority: Option<String>,
    },
}

#[derive(Subcommand)]
enum GroupCommand {
    Add {
        directory: String,
        name: String,
        #[arg(long)]
        priority: Option<String>,
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
        #[arg(long)]
        priority: Option<String>,
        #[arg(long = "link")]
        links: Vec<String>,
    },
    Complete {
        id: i64,
        #[arg(long)]
        note: Option<String>,
    },
    Move {
        id: i64,
        directory: String,
        #[arg(long)]
        group: Option<String>,
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
        Some(Command::Seed) => {
            store.seed().await?;
            println!("Seeded the Actum workspace.");
        }
        Some(Command::List { directory, all }) => {
            print_snapshot(&store.snapshot(&directory, all).await?);
        }
        Some(Command::Directory { command }) => match command {
            DirectoryCommand::Add { path, priority } => {
                let id = store
                    .ensure_directory(&path, parse_priority(priority.as_deref())?)
                    .await?;
                println!("Created or updated directory #{id}: {path}");
            }
        },
        Some(Command::Group { command }) => match command {
            GroupCommand::Add {
                directory,
                name,
                priority,
                links,
            } => {
                let group = store
                    .create_group(
                        &directory,
                        &name,
                        parse_priority(priority.as_deref())?,
                        &links,
                    )
                    .await?;
                println!(
                    "{} {}",
                    format_priority(group.effective_priority),
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
                priority,
                links,
            } => {
                let task = store
                    .create_task(
                        &directory,
                        group.as_deref(),
                        &title,
                        parse_priority(priority.as_deref())?,
                        &links,
                    )
                    .await?;
                println!(
                    "#{} {} {}",
                    task.id,
                    format_priority(task.effective_priority),
                    task.title
                );
                print_links(&task.links, "  ");
            }
            TaskCommand::Complete { id, note } => {
                let task = store.complete_task(id, note.as_deref()).await?;
                println!("Completed #{}: {}", task.id, task.title);
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
            format_priority(directory.effective_priority),
            directory.name
        );
    }
    for group in &snapshot.groups {
        println!(
            "\n  {} {} ({})",
            format_priority(group.group.effective_priority),
            group.group.name,
            group.tasks.len()
        );
        print_links(&group.group.links, "    ");
        for task in &group.tasks {
            let marker = if task.status == "completed" { "x" } else { " " };
            println!(
                "    [{marker}] #{} {} {}",
                task.id,
                format_priority(task.effective_priority),
                task.title
            );
            print_links(&task.links, "        ");
        }
    }
    if !snapshot.ungrouped_tasks.is_empty() {
        println!("\n  Ungrouped");
        for task in &snapshot.ungrouped_tasks {
            let marker = if task.status == "completed" { "x" } else { " " };
            println!(
                "    [{marker}] #{} {} {}",
                task.id,
                format_priority(task.effective_priority),
                task.title
            );
            print_links(&task.links, "        ");
        }
    }
}

fn print_links(links: &[String], indentation: &str) {
    for link in links {
        println!("{indentation}{link}");
    }
}
