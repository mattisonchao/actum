# Actum

Actum is a terminal-first task manager. PostgreSQL stores a directory → group → task hierarchy, a small TUI renders the live database in a terminal sidebar, and an MCP server gives AI agents a typed interface to the same operations.

Actum does not emulate a terminal. Keep your normal shell on the left and run `actum` in a terminal or multiplexer split on the right.

## First run

Requirements: Rust 1.92 or newer and Docker.

```bash
git clone https://github.com/mattisonchao/actum.git
cd actum
docker compose up -d --wait postgres
cargo run -- migrate
cargo run -- seed
cargo run -- list /projects/oxia
```

The database listens only on `127.0.0.1:55432`. Its data persists in the `actum-postgres-data` Docker volume.

## Open the sidebar

Create a right-side split in your terminal, then run:

```bash
actum
```

The left pane remains a native shell and can run any command or AI agent.

Sidebar keys:

- `Tab`: switch between directories and tasks
- `j` / `k` or arrow keys: move
- `Enter`: enter a directory or open the selected task's first link
- `Backspace`, `h`, or left arrow: move to the parent directory
- `Space`: complete the selected task
- `r`: refresh
- `q`: quit

Completed tasks are hidden by default. The sidebar also refreshes once per second after changes made by another CLI or MCP process.

## CLI examples

```bash
# Show pending tasks and raw links
cargo run -- list /projects/sql-workspace

# Create hierarchy
cargo run -- directory add /projects/example --priority P2
cargo run -- group add /projects/example "Public preview" --priority P2
cargo run -- task add /projects/example "Deploy the cluster" \
  --group "Public preview" --priority P1 \
  --link https://github.com/example/project/issues/1

# Move and complete tasks by stable ID
cargo run -- task move 1 /projects/example --group "Public preview"
cargo run -- task complete 1 --note "Verified in production"

# Include completed tasks
cargo run -- list /projects/example --all
```

Priorities can be `P1`, `P2`, or `P3`. Missing task priority inherits from its group, then its directory.

## MCP server

Build the binary and configure an MCP client to launch it over stdio:

```bash
cargo build --release
./target/release/actum mcp
```

Example client configuration:

```json
{
  "mcpServers": {
    "actum": {
      "command": "/absolute/path/to/actum/target/release/actum",
      "args": ["mcp"],
      "env": {
        "DATABASE_URL": "postgres://actum:actum@127.0.0.1:55432/actum"
      }
    }
  }
}
```

Available MCP tools:

- `list_items`
- `create_directory`
- `create_group`
- `create_task`
- `move_task`
- `complete_task`

The server advertises its own task-management instructions; no Actum-specific `AGENTS.md` is required.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

See [docs/architecture.md](docs/architecture.md) for the boundaries and data model.
