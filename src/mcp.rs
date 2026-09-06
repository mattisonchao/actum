use anyhow::Result;
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt, handler::server::wrapper::Parameters,
    schemars, tool, tool_handler, tool_router, transport::stdio,
};
use serde::Deserialize;

use crate::{model::parse_priority, store::Store};

#[derive(Clone)]
pub struct ActumMcp {
    store: Store,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListItems {
    /// Absolute Actum directory path. Defaults to /projects.
    directory: Option<String>,
    /// Include completed tasks. Defaults to false.
    include_completed: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateDirectory {
    /// Absolute nested directory path, for example /projects/oxia.
    path: String,
    /// Optional P1, P2, or P3 priority.
    priority: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateGroup {
    /// Existing absolute directory path.
    directory: String,
    /// Group name unique within the directory.
    name: String,
    /// Optional P1, P2, or P3 priority.
    priority: Option<String>,
    /// Raw external links associated with the group.
    links: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateTask {
    /// Existing absolute directory path.
    directory: String,
    /// Optional group in the same directory. Omit for an ungrouped task.
    group: Option<String>,
    /// Actionable task title.
    title: String,
    /// Optional P1, P2, or P3 priority.
    priority: Option<String>,
    /// Raw external links associated with the task.
    links: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CompleteTask {
    /// Stable numeric task ID returned by Actum.
    task_id: i64,
    /// Optional explanation of the completed outcome.
    completion_note: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MoveTask {
    /// Stable numeric task ID returned by Actum.
    task_id: i64,
    /// Existing target directory path.
    directory: String,
    /// Optional target group. Omit to make the task ungrouped.
    group: Option<String>,
}

#[tool_router]
impl ActumMcp {
    fn new(store: Store) -> Self {
        Self { store }
    }

    #[tool(
        description = "List directories, groups, and tasks. Completed tasks are hidden by default. Returns stable IDs, effective priorities, statuses, and raw links."
    )]
    async fn list_items(
        &self,
        Parameters(input): Parameters<ListItems>,
    ) -> Result<String, McpError> {
        let snapshot = self
            .store
            .snapshot(
                input.directory.as_deref().unwrap_or("/projects"),
                input.include_completed.unwrap_or(false),
            )
            .await
            .map_err(mcp_error)?;
        serde_json::to_string_pretty(&snapshot).map_err(mcp_error)
    }

    #[tool(
        description = "Create a nested directory or update its explicit priority. Parent directories are created automatically."
    )]
    async fn create_directory(
        &self,
        Parameters(input): Parameters<CreateDirectory>,
    ) -> Result<String, McpError> {
        let priority = parse_priority(input.priority.as_deref()).map_err(mcp_error)?;
        let id = self
            .store
            .ensure_directory(&input.path, priority)
            .await
            .map_err(mcp_error)?;
        Ok(serde_json::json!({ "id": id, "path": input.path }).to_string())
    }

    #[tool(
        description = "Create a group inside an existing directory. Reusing the same name updates its supplied priority and links."
    )]
    async fn create_group(
        &self,
        Parameters(input): Parameters<CreateGroup>,
    ) -> Result<String, McpError> {
        let priority = parse_priority(input.priority.as_deref()).map_err(mcp_error)?;
        let group = self
            .store
            .create_group(
                &input.directory,
                &input.name,
                priority,
                &input.links.unwrap_or_default(),
            )
            .await
            .map_err(mcp_error)?;
        serde_json::to_string_pretty(&group).map_err(mcp_error)
    }

    #[tool(
        description = "Create a task in an existing directory and optional group. Reusing the same title in that directory updates supplied fields and preserves omitted links."
    )]
    async fn create_task(
        &self,
        Parameters(input): Parameters<CreateTask>,
    ) -> Result<String, McpError> {
        let priority = parse_priority(input.priority.as_deref()).map_err(mcp_error)?;
        let task = self
            .store
            .create_task(
                &input.directory,
                input.group.as_deref(),
                &input.title,
                priority,
                &input.links.unwrap_or_default(),
            )
            .await
            .map_err(mcp_error)?;
        serde_json::to_string_pretty(&task).map_err(mcp_error)
    }

    #[tool(
        description = "Mark one task complete by stable ID. Call only after the user confirms completion or an agreed completion condition is satisfied."
    )]
    async fn complete_task(
        &self,
        Parameters(input): Parameters<CompleteTask>,
    ) -> Result<String, McpError> {
        let task = self
            .store
            .complete_task(input.task_id, input.completion_note.as_deref())
            .await
            .map_err(mcp_error)?;
        serde_json::to_string_pretty(&task).map_err(mcp_error)
    }

    #[tool(description = "Move one task by stable ID to an existing directory and optional group.")]
    async fn move_task(&self, Parameters(input): Parameters<MoveTask>) -> Result<String, McpError> {
        let task = self
            .store
            .move_task(input.task_id, &input.directory, input.group.as_deref())
            .await
            .map_err(mcp_error)?;
        serde_json::to_string_pretty(&task).map_err(mcp_error)
    }
}

#[tool_handler(
    name = "actum",
    version = "0.1.0",
    instructions = "Actum manages a directory → group → task hierarchy. Use stable IDs for mutations, preserve raw links, treat P1 as highest priority, hide completed tasks unless requested, and never infer completion from external link status alone."
)]
impl ServerHandler for ActumMcp {}

pub async fn run(store: Store) -> Result<()> {
    let server = ActumMcp::new(store).serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}

fn mcp_error(error: impl std::fmt::Display) -> McpError {
    McpError::internal_error(error.to_string(), None)
}
