//! MCP server: 10 tools over stdio + HTTP/SSE (playbook §10).
//!
//! STUB for Phase 0-4: the rmcp 0.1 API needs deeper integration work.
//! Full implementation lands in Phase 5. The ToolResult envelope and
//! parameter structs below define the contract.

use crate::state::SharedState;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Tool result envelope: always includes token estimate.
#[derive(Debug, Serialize)]
pub struct ToolResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Estimated tokens in the response (for budget tracking)
    pub tokens_est: usize,
}

impl ToolResult {
    pub fn ok(data: serde_json::Value) -> Self {
        let tokens_est = data.to_string().len() / 4;
        Self {
            ok: true,
            data: Some(data),
            error: None,
            tokens_est,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(msg.into()),
            tokens_est: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Tool parameter structs (contract for Phase 5)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct NavigateParams {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct ClickRefParams {
    pub r#ref: u32,
}

#[derive(Debug, Deserialize)]
pub struct FillRefParams {
    pub r#ref: u32,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotParams {
    #[serde(default = "default_max_nodes")]
    pub max_nodes: usize,
}

fn default_max_nodes() -> usize {
    200
}

#[derive(Debug, Deserialize)]
pub struct DiffParams {
    #[serde(default)]
    pub drain: bool,
}

#[derive(Debug, Deserialize)]
pub struct OrderParams {
    pub instruction: String,
}

#[derive(Debug, Deserialize)]
pub struct UploadParams {
    pub r#ref: u32,
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct EvalParams {
    pub js: String,
}

/// Phase 5: wire these into rmcp ToolRouter.
#[allow(dead_code)]
pub struct BridgeMcp {
    state: SharedState,
}

#[allow(dead_code)]
impl BridgeMcp {
    pub fn new(state: SharedState) -> Self {
        Self { state }
    }

    pub async fn tabs(&self) -> ToolResult {
        let tabs = self.state.tabs.read().await;
        ToolResult::ok(json!({"tabs": tabs.list()}))
    }
}
