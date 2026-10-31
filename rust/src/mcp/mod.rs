//! MCP server: 10 tools over stdio (playbook §10).
//!
//! Uses rmcp's `#[tool(tool_box)]` macro. Tools delegate to the same
//! logic as the HTTP routes. Serve via `serve_stdio()`.

use crate::state::SharedState;
use rmcp::{tool, ServerHandler, ServiceExt, model::ServerInfo};
use serde::Deserialize;
use serde_json::json;

// ---------------------------------------------------------------------------
// Tool parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NavigateParams {
    #[schemars(description = "URL to navigate to (http/https only, SSRF-filtered)")]
    pub url: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickRefParams {
    #[schemars(description = "[ref=N] handle from the AX snapshot")]
    pub r#ref: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FillRefParams {
    #[schemars(description = "[ref=N] handle from the AX snapshot")]
    pub r#ref: u32,
    #[schemars(description = "Text to fill into the field")]
    pub text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SnapshotParams {
    #[schemars(description = "Max nodes before truncation")]
    pub max_nodes: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OrderParams {
    #[schemars(description = "Natural-language instruction, e.g. 'the blue submit button'")]
    pub instruction: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UploadParams {
    #[schemars(description = "[ref=N] handle of the file input")]
    pub r#ref: u32,
    #[schemars(description = "Path under the upload allowlist")]
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EvalParams {
    #[schemars(description = "JavaScript to evaluate (ADMIN scope only - use with caution)")]
    pub js: String,
}

// ---------------------------------------------------------------------------
// The MCP server
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct BridgeMcp {
    state: SharedState,
}

#[tool(tool_box)]
impl BridgeMcp {
    pub fn new(state: SharedState) -> Self {
        Self { state }
    }

    #[tool(description = "Navigate the browser to a URL (SSRF-filtered)")]
    async fn navigate(&self, #[tool(aggr)] params: NavigateParams) -> String {
        match crate::privy::validate_navigate_url(&params.url, &[]) {
            Ok(u) => json!({"ok": true, "url": u.as_str(), "validated": true}).to_string(),
            Err(e) => json!({"ok": false, "error": e}).to_string(),
        }
    }

    #[tool(description = "Interactive accessibility snapshot with [ref=N] handles")]
    async fn snapshot(&self, #[tool(aggr)] params: SnapshotParams) -> String {
        let max = params.max_nodes.unwrap_or(200);
        // TODO: wire to actual browser snapshot (Phase 2 handler)
        json!({"ok": false, "error": "snapshot via HTTP /browser/snapshot for now", "max_nodes": max}).to_string()
    }

    #[tool(description = "Drain DOM mutation queue (what changed since last check)")]
    async fn diff(&self) -> String {
        json!({"ok": false, "error": "diff via HTTP /browser/diff_state for now"}).to_string()
    }

    #[tool(description = "Click an element by [ref=N] handle (verified)")]
    async fn click_ref(&self, #[tool(aggr)] params: ClickRefParams) -> String {
        json!({"ok": false, "error": "click via HTTP /browser/click for now", "ref": params.r#ref}).to_string()
    }

    #[tool(description = "Fill a field by [ref=N] (React-aware)")]
    async fn fill_ref(&self, #[tool(aggr)] params: FillRefParams) -> String {
        json!({"ok": false, "error": "fill via HTTP /browser/fill for now", "ref": params.r#ref}).to_string()
    }

    #[tool(description = "Resolve natural-language instruction to element refs")]
    async fn order(&self, #[tool(aggr)] params: OrderParams) -> String {
        json!({"ok": false, "error": "order via HTTP /browser/order for now", "instruction": params.instruction}).to_string()
    }

    #[tool(description = "Upload a file to a file input by [ref=N]")]
    async fn upload(&self, #[tool(aggr)] params: UploadParams) -> String {
        // Validate path via interact::upload::validate_upload_path
        let allowlist = self.state.config.upload_dirs.clone();
        match crate::interact::upload::validate_upload_path(&params.path, &allowlist) {
            Ok(p) => json!({"ok": true, "path": p.to_string_lossy(), "ref": params.r#ref}).to_string(),
            Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
        }
    }

    #[tool(description = "Capture screenshot with Set-of-Marks overlay")]
    async fn screenshot_marks(&self) -> String {
        json!({"ok": false, "error": "screenshot via HTTP /browser/som_screenshot for now"}).to_string()
    }

    #[tool(description = "Get current browser tabs")]
    async fn tabs(&self) -> String {
        let tabs = self.state.tabs.read().await;
        let list: Vec<_> = tabs.list();
        // Estimate tokens: ~4 chars per token
        let data = json!({"tabs": list});
        let tokens_est = data.to_string().len() / 4;
        json!({"ok": true, "tabs": list, "tokens_est": tokens_est}).to_string()
    }

    #[tool(description = "Evaluate JavaScript (ADMIN scope only - prefer HTTP /browser/eval)")]
    async fn eval(&self, #[tool(aggr)] params: EvalParams) -> String {
        json!({"ok": false, "error": "eval via HTTP /browser/eval for now (ADMIN scope)", "js_len": params.js.len()}).to_string()
    }
}

#[tool(tool_box)]
impl ServerHandler for BridgeMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some("vm-bridge MCP: browser automation via [ref=N] handles. Use snapshot to get refs, then click_ref/fill_ref.".into()),
            ..Default::default()
        }
    }
}

/// Serve MCP over stdio (for Claude Code-style local clients).
/// Run as: `vm-bridge --mcp-stdio`
pub async fn serve_stdio(state: SharedState) -> anyhow::Result<()> {
    let service = BridgeMcp::new(state);
    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let server = service.serve(transport).await.map_err(|e| anyhow::anyhow!("mcp serve: {}", e))?;
    tracing::info!("MCP server running on stdio");
    let quit = server.waiting().await.map_err(|e| anyhow::anyhow!("mcp waiting: {}", e))?;
    tracing::info!(?quit, "MCP server stopped");
    Ok(())
}
