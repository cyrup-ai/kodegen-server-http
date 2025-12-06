use kodegen_mcp_schema::Tool;
use rmcp::handler::server::router::{prompt::PromptRouter, tool::ToolRouter};
use std::sync::Arc;

// Import log for tool registration logging
use log;

/// Register a single tool with both routers
///
/// Takes ownership of the tool, wraps it in Arc once, clones that Arc for both routes.
/// This ensures the tool instance is shared between tool and prompt routers.
///
/// Example usage:
/// ```no_run
/// # use kodegen_server_http::register_tool;
/// # use rmcp::handler::server::router::{prompt::PromptRouter, tool::ToolRouter};
/// # use kodegen_config_manager::ConfigManager;
/// # use kodegen_mcp_schema::{Tool, ToolExecutionContext};
/// # use kodegen_mcp_schema::McpError;
/// # use rmcp::model::{Content, PromptArgument, PromptMessage};
/// # use serde_json::Value;
/// #
/// # #[derive(Clone)]
/// # struct ReadFileTool { config: ConfigManager }
/// # impl ReadFileTool {
/// #     fn new(_limit: usize, config: ConfigManager) -> Self { Self { config } }
/// # }
/// # impl Tool for ReadFileTool {
/// #     type Args = Value;
/// #     type PromptArgs = Value;
/// #     fn name() -> &'static str { "fs_read_file" }
/// #     fn description() -> &'static str { "Read file" }
/// #     async fn execute(&self, _args: Self::Args, _ctx: ToolExecutionContext) -> Result<Vec<Content>, McpError> {
/// #         Ok(vec![])
/// #     }
/// #     fn prompt_arguments() -> Vec<PromptArgument> { vec![] }
/// #     async fn prompt(&self, _args: Self::PromptArgs) -> Result<Vec<PromptMessage>, McpError> {
/// #         Ok(vec![])
/// #     }
/// # }
/// #
/// # fn main() {
/// # let tool_router = ToolRouter::<()>::new();
/// # let prompt_router = PromptRouter::<()>::new();
/// # let config = ConfigManager::new();
/// let (tool_router, prompt_router) = register_tool(
///     tool_router, prompt_router,
///     ReadFileTool::new(2000, config.clone()),
/// );
/// # }
/// ```
pub fn register_tool<S, T>(
    tool_router: ToolRouter<S>,
    prompt_router: PromptRouter<S>,
    tool: T,
) -> (ToolRouter<S>, PromptRouter<S>)
where
    S: Send + Sync + 'static,
    T: Tool,
{
    let tool_name = T::name();
    
    log::info!("Registering tool: {}", tool_name);
    
    let tool = Arc::new(tool);
    let tool_router = tool_router.with_route(tool.clone().arc_into_tool_route());
    let prompt_router = prompt_router.with_route(tool.arc_into_prompt_route());
    
    log::info!("✓ Successfully registered tool: {}", tool_name);
    
    (tool_router, prompt_router)
}

/// Register an already-Arc-wrapped tool
///
/// Use this when you need to hold a reference to the tool for cleanup tasks.
/// Avoids creating the tool twice - uses the same Arc for both registration and cleanup.
///
/// Example usage:
/// ```no_run
/// # use std::sync::Arc;
/// # use kodegen_server_http::register_tool_arc;
/// # use rmcp::handler::server::router::{prompt::PromptRouter, tool::ToolRouter};
/// # use kodegen_mcp_schema::{Tool, ToolExecutionContext};
/// # use kodegen_mcp_schema::McpError;
/// # use rmcp::model::{Content, PromptArgument, PromptMessage};
/// # use serde_json::Value;
/// #
/// # struct SequentialThinkingTool;
/// # impl SequentialThinkingTool {
/// #     fn new() -> Self { Self }
/// #     fn start_cleanup_task(self: Arc<Self>) {}
/// # }
/// # impl Tool for SequentialThinkingTool {
/// #     type Args = Value;
/// #     type PromptArgs = Value;
/// #     fn name() -> &'static str { "sequential_thinking" }
/// #     fn description() -> &'static str { "Sequential thinking" }
/// #     async fn execute(&self, _args: Self::Args, _ctx: ToolExecutionContext) -> Result<Vec<Content>, McpError> {
/// #         Ok(vec![])
/// #     }
/// #     fn prompt_arguments() -> Vec<PromptArgument> { vec![] }
/// #     async fn prompt(&self, _args: Self::PromptArgs) -> Result<Vec<PromptMessage>, McpError> {
/// #         Ok(vec![])
/// #     }
/// # }
/// #
/// # fn main() {
/// # let tool_router = ToolRouter::<()>::new();
/// # let prompt_router = PromptRouter::<()>::new();
/// let thinking_tool = Arc::new(SequentialThinkingTool::new());
/// let (tool_router, prompt_router) = register_tool_arc(
///     tool_router, prompt_router,
///     thinking_tool.clone(),
/// );
/// // Hold reference for background task cleanup
/// thinking_tool.clone().start_cleanup_task();
/// # }
/// ```
pub fn register_tool_arc<S, T>(
    tool_router: ToolRouter<S>,
    prompt_router: PromptRouter<S>,
    tool: Arc<T>,
) -> (ToolRouter<S>, PromptRouter<S>)
where
    S: Send + Sync + 'static,
    T: Tool,
{
    let tool_name = T::name();
    
    log::info!("Registering tool (Arc): {}", tool_name);
    
    let tool_router = tool_router.with_route(tool.clone().arc_into_tool_route());
    let prompt_router = prompt_router.with_route(tool.arc_into_prompt_route());
    
    log::info!("✓ Successfully registered tool (Arc): {}", tool_name);
    
    (tool_router, prompt_router)
}
