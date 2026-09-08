# Actum

Actum is a terminal-first task manager. PostgreSQL stores a directory → group → task hierarchy, a small TUI renders the live database in a terminal sidebar, and an MCP server gives AI agents a typed interface to the same operations.

Actum runs only the task sidebar. Your terminal owns the panes, so the shell beside Actum remains a completely native terminal.

## First run

Requirements: Rust 1.92 or newer and Docker.

```bash
git clone https://github.com/mattisonchao/actum.git
cd actum
docker compose up -d --wait postgres
cargo run -- migrate
cargo run -- init
cargo run -- list /projects
```

The database listens only on `127.0.0.1:55432`. Its data persists in the `actum-postgres-data` Docker volume.

## Start Actum

In Ghostty, press `⌘D` to create a native right split, then run one command in that pane:

```bash
actum
```

The left pane remains your native shell and can run any command or AI agent. The right pane reads the same PostgreSQL data used by the CLI and MCP server. Other terminal applications work too; use their normal split-pane action.

Inside Actum, the upper tabs switch between `Today`, `Backlog`, and `Completed`, with live task counts. `Today` is the default and includes open tasks due today or overdue. `Backlog` shows all open work. `Completed` provides directory and finish-date archive views with full completion details.

Below the tabs, the top row shows directories on the left and the selected directory's task list on the right. Colored group badges make task sections easy to scan. The bottom row shows the selected task's unique ID, status, deadline, location, group, completion time, links, and comments or notes.

Sidebar keys:

- `Tab`: switch between directories and tasks
- `1` / `2` / `3`: open Today, Backlog, or Completed
- `[` / `]`: switch to the previous or next tab
- `d` / `f` in Completed: archive by directory or finish date
- `j` / `k` or arrow keys: move
- `Enter`: enter a directory or open the selected task's first link
- `Backspace`, `h`, or left arrow: move to the parent directory
- `Space`: complete the selected task
- `r`: refresh
- `q`: quit Actum in the task pane

The sidebar refreshes once per second after changes made by another CLI or MCP process.

Each project can keep completed work in a nested `finished/` directory. Actum recognizes that directory as an archive in the TUI, CLI, and MCP server. The Completed tab also finds completed tasks recursively, so its directory and finish-date views include the full archive with original groups, stable IDs, links, deadlines, completion timestamps, and notes.

## CLI examples

```bash
# Show pending tasks and raw links
cargo run -- list /projects/sql-workspace

# Create hierarchy
cargo run -- directory add /projects/example --deadline 2030-06-30
cargo run -- group add /projects/example "Public preview" --deadline 2030-06-15
cargo run -- task add /projects/example "Deploy the cluster" \
  --group "Public preview" --deadline 2030-06-01 \
  --link https://github.com/example/project/issues/1

# Move and complete tasks by stable ID
cargo run -- task move 1 /projects/example --group "Public preview"
cargo run -- task deadline 1 2030-06-01
cargo run -- task complete 1 --note "Verified in production"

# Include completed tasks
cargo run -- list /projects/example --all
```

Deadlines use `YYYY-MM-DD`. A missing task deadline inherits from its group, then its directory. Tasks sort by deadline, and each group or directory rolls up its earliest open-task deadline so urgent work rises through the dashboard; undated items sort last.

## Connect an AI client with MCP

Actum includes a local MCP server. Configure your AI client to launch the installed binary over stdio; Actum and the sidebar will then share the same PostgreSQL tasks.

### Codex

Add Actum with the Codex CLI:

```bash
codex mcp add actum \
  --env DATABASE_URL=postgres://actum:actum@127.0.0.1:55432/actum \
  -- "$(command -v actum)" mcp
```

Restart Codex after adding the server. You can confirm the connection with `/mcp` in Codex or `codex mcp list`. See the [official Codex MCP documentation](https://developers.openai.com/codex/mcp).

### JSON-based clients

Use the absolute path reported by `command -v actum` as `command`. For example:

```json
{
  "mcpServers": {
    "actum": {
      "command": "/Users/you/.cargo/bin/actum",
      "args": ["mcp"],
      "env": {
        "DATABASE_URL": "postgres://actum:actum@127.0.0.1:55432/actum"
      }
    }
  }
}
```

Restart or reload the AI client after adding the server. It can then discover these tools and manage the same data shown in the sidebar:

- `list_items`
- `create_directory`
- `create_group`
- `create_task`
- `move_task`
- `set_task_deadline`
- `complete_task`
- `delete_task` — permanently removes a task only when explicitly requested

The server advertises its task-management instructions and tool schemas during the MCP handshake; no Actum-specific `AGENTS.md` is required.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

See [docs/architecture.md](docs/architecture.md) for the boundaries and data model.
