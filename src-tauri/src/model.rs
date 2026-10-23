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
    #[serde(default)]
    pub started_at: Option<u64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub attempts: u32,
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
    /// Bounded graph snapshot for inspection/recreation only. Older history records omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Automation>,
    /// Bounded per-block records; defaults empty for older history files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<NodeTrace>,
    /// True when the trace cap was reached and older step records were discarded.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trace_truncated: bool,
}


#[cfg(test)]
mod run_trace_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_history_records_load_without_trace_or_snapshot() {
        let record: RunRecord = serde_json::from_value(json!({
            "id": 12,
            "startedAt": 100,
            "endedAt": 150,
            "outcome": "completed",
            "errors": 0
        })).expect("legacy run history should remain readable");
        assert!(record.snapshot.is_none());
        assert!(record.trace.is_empty());
        assert!(!record.trace_truncated);
    }

    #[test]
    fn trace_fields_round_trip_in_camel_case() {
        let record = RunRecord {
            id: 2,
            started_at: 10,
            ended_at: 21,
            outcome: "failed".into(),
            errors: 1,
            snapshot: None,
            trace: vec![NodeTrace {
                automation_id: "auto-1".into(),
                node_id: "node-2".into(),
                block_title: "Run Command".into(),
                state: "failed".into(),
                detail: "exit code 1".into(),
                visit: 3,
                duration_ms: 11,
            }],
            trace_truncated: true,
        };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["trace"][0]["nodeId"], "node-2");
        assert_eq!(value["trace"][0]["durationMs"], 11);
        assert_eq!(value["traceTruncated"], true);
        let decoded: RunRecord = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.trace[0].visit, 3);
        assert!(decoded.trace_truncated);
    }
}
