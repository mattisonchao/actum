# Architecture

## Process model

```text
terminal window
├── native shell                         any shell command or AI agent
└── actum                                read and update task state

AI client ── MCP over stdio ──┐
actum CLI ────────────────────┼── shared Rust operations ── PostgreSQL
actum ────────────────────────┘
```

Actum does not capture shell input, proxy commands, create splits, or implement terminal emulation. The user's terminal application owns the layout and native shell; Actum renders only inside the pane where it is started.

The TUI uses one page with three filtered tabs and two content rows:

```text
┌─ Today ─┬─ Backlog ─┬─ Completed ────────────────────┐
│                         └─ Directory / Finish date   │
├─ directories ─────┬─ tasks ─────────────────────────┤
│ project tree      │ compact list with unique IDs    │
├───────────────────┴─────────────────────────────────┤
│ selected task details, links, comments, and notes   │
└─────────────────────────────────────────────────────┘
```

## Hierarchy

```text
directory/
├── group
│   ├── task
│   └── task
└── ungrouped task
```

- Directories may nest.
- Groups belong to exactly one directory.
- Tasks belong to one directory and optionally one group in that directory.
- Directories, groups, and tasks can set a `YYYY-MM-DD` deadline.
- A missing deadline inherits from the closest ancestor. Tasks sort by deadline, while groups and directories roll up the earliest deadline from their open descendants; undated items sort last.
- Completed tasks are hidden by default.
- A project's nested `finished/` directory preserves completed task IDs and groups. The shared store recognizes that path as an archive, so the TUI, CLI, and MCP server show completed items there automatically.
- The Today view includes open tasks with an effective deadline on or before the local date; Backlog includes all open tasks.
- The Completed view recursively loads full completion data and can organize it by project directory or completion date.
- Groups and tasks retain raw external links.

## Interfaces

The CLI, TUI, and MCP handlers call the same `Store` application operations. MCP exposes narrow task-management tools and never unrestricted SQL.

The MCP server publishes behavioral instructions with its server metadata and detailed tool schemas. Important invariants remain enforced by PostgreSQL constraints and the shared application layer rather than relying on agent instructions.

## First-version tradeoffs

- The sidebar polls once per second for cross-process changes. PostgreSQL `LISTEN/NOTIFY` can replace polling without changing the UI.
- The code is a single Rust crate. It can split into core, PostgreSQL, CLI/TUI, and MCP crates when those boundaries need independent releases.
- PostgreSQL runs locally in Docker and binds only to loopback. The connection URL can later target a cloud database without changing commands.
