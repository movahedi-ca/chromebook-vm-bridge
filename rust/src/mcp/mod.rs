//! MCP server: 10 tools over stdio (playbook §10).
//!
//! Uses rmcp's `#[tool(tool_box)]` macro. Tools delegate to the same
//! logic as the HTTP routes. Serve via `serve_stdio()`.

#![allow(clippy::unused_async)] // rmcp ServerHandler requires async tool fns; bodies are sync by design
#![allow(clippy::unused_async_trait_impl)] // same: the trait mandates async even when the impl does not await
use crate::state::SharedState;
use rmcp::{model::ServerInfo, tool, ServerHandler, ServiceExt};
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
    #[allow(dead_code)] // parsed but unused while fill_ref is a guidance stub (audit MED-7)
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

/// Whether the MCP process environment authorizes browser tools
/// (audit 42 High-9).
///
/// MCP stdio has no transport auth, no lease enforcement, and no rate
/// limiting — unlike the HTTP API. The gate is possession of the secret:
/// the MCP server process must have been spawned with `BRIDGE_API_KEY`
/// or the scoped `VM_BRIDGE_KEYS__ACT` in its environment. Without it,
/// every browser tool refuses with a clear error instead of silently
/// operating unauthenticated.
fn mcp_env_authorized() -> bool {
    let present = |key: &str| std::env::var(key).is_ok_and(|v| !v.trim().is_empty());
    present("BRIDGE_API_KEY") || present("VM_BRIDGE_KEYS__ACT")
}

fn mcp_auth_refusal(tool: &str) -> String {
    json!({
        "ok": false,
        "error": format!(
            "{tool} refused: MCP browser tools require BRIDGE_API_KEY or \
             VM_BRIDGE_KEYS__ACT in the MCP server process environment"
        ),
    })
    .to_string()
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

    #[tool(
        description = "Navigate the browser to a URL (SSRF-filtered). Requires BRIDGE_API_KEY or VM_BRIDGE_KEYS__ACT in the MCP server process environment."
    )]
    async fn navigate(&self, #[tool(aggr)] params: NavigateParams) -> String {
        use axum::response::IntoResponse;
        if !mcp_env_authorized() {
            return mcp_auth_refusal("navigate");
        }
        // Delegate to the HTTP /browser/navigate handler (audit 40 HIGH-4):
        // this tool actually navigates now instead of merely validating.
        let resp = crate::routes::browser::navigate(
            axum::extract::State(self.state.clone()),
            axum::Json(crate::routes::browser::NavigateRequest { url: params.url }),
        )
        .await
        .into_response();
        match axum::body::to_bytes(resp.into_body(), 1024 * 1024).await {
            Ok(b) => String::from_utf8_lossy(&b).to_string(),
            Err(e) => json!({"ok": false, "error": format!("navigate response: {e}")}).to_string(),
        }
    }

    #[tool(
        description = "NOT IMPLEMENTED over MCP: use HTTP GET /browser/snapshot for the interactive accessibility snapshot"
    )]
    async fn snapshot(&self, #[tool(aggr)] params: SnapshotParams) -> String {
        let max = params.max_nodes.unwrap_or(200);
        // TRACKED DECISION (was bare TODO): MCP snapshot stays a guidance
        // stub until the MCP tools are wired to the real handlers en masse
        // (audit 40 HIGH-4 did navigate; the rest follow the same delegate
        // pattern when prioritized). Honest ok:false, not a silent stub.
        json!({"ok": false, "error": "snapshot via HTTP /browser/snapshot for now", "max_nodes": max}).to_string()
    }

    #[tool(
        description = "NOT IMPLEMENTED over MCP: use HTTP /browser/diff_state for the DOM mutation queue"
    )]
    async fn diff(&self) -> String {
        json!({"ok": false, "error": "diff via HTTP /browser/diff_state for now"}).to_string()
    }

    #[tool(
        description = "NOT IMPLEMENTED over MCP: use HTTP POST /browser/click to click by [ref=N] handle"
    )]
    async fn click_ref(&self, #[tool(aggr)] params: ClickRefParams) -> String {
        json!({"ok": false, "error": "click via HTTP /browser/click for now", "ref": params.r#ref})
            .to_string()
    }

    #[tool(
        description = "NOT IMPLEMENTED over MCP: use HTTP POST /browser/fill to fill by [ref=N]"
    )]
    async fn fill_ref(&self, #[tool(aggr)] params: FillRefParams) -> String {
        json!({"ok": false, "error": "fill via HTTP /browser/fill for now", "ref": params.r#ref})
            .to_string()
    }

    #[tool(
        description = "NOT IMPLEMENTED over MCP: use HTTP POST /browser/order for natural-language element resolution"
    )]
    async fn order(&self, #[tool(aggr)] params: OrderParams) -> String {
        json!({"ok": false, "error": "order via HTTP /browser/order for now", "instruction": params.instruction}).to_string()
    }

    #[tool(description = "Validate an upload path against the allowlist (upload itself via HTTP)")]
    async fn upload(&self, #[tool(aggr)] params: UploadParams) -> String {
        // Validate path via interact::upload::validate_upload_path
        let allowlist = self.state.config.upload_dirs.clone();
        match crate::interact::upload::validate_upload_path(&params.path, &allowlist) {
            Ok(p) => {
                json!({"ok": true, "path": p.to_string_lossy(), "ref": params.r#ref}).to_string()
            }
            Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
        }
    }

    #[tool(
        description = "NOT IMPLEMENTED over MCP: use HTTP /browser/som_screenshot for the Set-of-Marks screenshot"
    )]
    async fn screenshot_marks(&self) -> String {
        json!({"ok": false, "error": "screenshot via HTTP /browser/som_screenshot for now"})
            .to_string()
    }

    #[tool(
        description = "Get current browser tabs. Requires BRIDGE_API_KEY or VM_BRIDGE_KEYS__ACT in the MCP server process environment."
    )]
    async fn tabs(&self) -> String {
        if !mcp_env_authorized() {
            return mcp_auth_refusal("tabs");
        }
        let tabs = self.state.tabs.read().await;
        let list: Vec<_> = tabs.list();
        // Estimate tokens: ~4 chars per token
        let data = json!({"tabs": list});
        let tokens_est = data.to_string().len() / 4;
        json!({"ok": true, "tabs": list, "tokens_est": tokens_est}).to_string()
    }

    #[tool(
        description = "NOT IMPLEMENTED over MCP: use HTTP POST /browser/eval (ADMIN scope) to evaluate JavaScript"
    )]
    async fn eval(&self, #[tool(aggr)] params: EvalParams) -> String {
        json!({"ok": false, "error": "eval via HTTP /browser/eval for now (ADMIN scope)", "js_len": params.js.len()}).to_string()
    }
}

#[tool(tool_box)]
impl ServerHandler for BridgeMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            // Honest about what is live over MCP (audit 42 High-9): only
            // navigate and tabs execute here (gated on the process
            // environment); every other tool is a pointer to its HTTP
            // equivalent, not a working tool.
            instructions: Some(
                "vm-bridge MCP: only `navigate` and `tabs` are live over MCP \
                 (they require BRIDGE_API_KEY or VM_BRIDGE_KEYS__ACT in the MCP \
                 server process environment). All other tools are unimplemented \
                 pointers — use their HTTP equivalents."
                    .into(),
            ),
            ..Default::default()
        }
    }
}

/// Serve MCP over stdio (for Claude Code-style local clients).
/// Run as: `vm-bridge --mcp-stdio`
pub async fn serve_stdio(state: SharedState) -> anyhow::Result<()> {
    if mcp_env_authorized() {
        tracing::info!("MCP stdio: browser tools authorized via process environment");
    } else {
        tracing::warn!(
            "MCP stdio: no BRIDGE_API_KEY or VM_BRIDGE_KEYS__ACT in process environment —              browser tools (navigate, tabs) will refuse"
        );
    }
    let service = BridgeMcp::new(state);
    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let server = service
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("mcp serve: {e}"))?;
    tracing::info!("MCP server running on stdio");
    let quit = server
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("mcp waiting: {e}"))?;
    tracing::info!(?quit, "MCP server stopped");
    Ok(())
}
