use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Node {
    pub id: String,
    pub kind: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub cfg: Value,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Edge {
    pub id: String,
    pub from: String,
    pub to: String,
    /// "if" | "else" | "after" (flow into a node, including a Target) or "target" (binds and passes through a Target to a Condition/Action)
    pub kind: String,
    #[serde(default)]
    pub bend: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Viewport {
    pub x: f64,
    pub y: f64,
    pub k: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Automation {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub viewport: Viewport,
    #[serde(default)]
    pub auto_start: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub seq: u64,
    pub ts: u64,
    pub level: String,
    pub auto_id: String,
    pub auto_name: String,
    pub node_id: Option<String>,
    pub node_label: Option<String>,
    pub msg: String,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NodeState {
    pub state: String,
    pub progress: Option<f64>,
    pub detail: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AutoRuntime {
    pub status: String,
    pub started_at: Option<u64>,
    pub last_run: Option<u64>,
    pub last_outcome: Option<String>,
}

impl Default for AutoRuntime {
    fn default() -> Self {
        AutoRuntime { status: "stopped".into(), started_at: None, last_run: None, last_outcome: None }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NodeTrace {
    pub automation_id: String,
    pub node_id: String,
    pub block_title: String,
    pub state: String,
    pub detail: String,
    pub visit: u64,
    pub duration_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub id: u64,
    pub started_at: u64,
    pub ended_at: u64,
    pub outcome: String,
    pub errors: u32,
    /// Per-node execution summaries; default keeps existing history files loadable.
    #[serde(default)]
    pub trace: Vec<NodeTrace>,
}
