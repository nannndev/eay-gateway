use crate::store::now;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ActivityMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub id: String,
    pub parent_id: Option<String>,
    pub agent: Option<String>,
    /// One of: queued, preparing, routing, waiting_upstream, thinking, generating, calling_tool, failover, completed, failed
    pub phase: String,
    pub message: String,
    pub target: Option<String>,
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<ActivityMeta>,
    pub ts: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentRun {
    pub id: String,
    pub parent_session: Option<String>,
    pub role: String,
    pub model: String,
    pub phase: String,
    pub status_message: String,
    pub target: Option<String>,
    pub tool: Option<String>,
    pub created_at: f64,
    pub updated_at: f64,
    pub output: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct ActivityHub {
    tx: broadcast::Sender<ActivityEvent>,
    recent: Arc<Mutex<VecDeque<ActivityEvent>>>,
    runs: Arc<Mutex<HashMap<String, SubagentRun>>>,
}

impl Default for ActivityHub {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { tx, recent: Arc::new(Mutex::new(VecDeque::with_capacity(100))), runs: Default::default() }
    }
}

impl ActivityHub {
    pub fn subscribe(&self) -> broadcast::Receiver<ActivityEvent> {
        self.tx.subscribe()
    }

    pub fn recent_events(&self) -> Vec<ActivityEvent> {
        self.recent.lock().iter().cloned().collect()
    }

    pub fn emit(&self, ev: ActivityEvent) {
        if let Some(run) = self.runs.lock().get_mut(&ev.id) {
            run.phase = ev.phase.clone();
            run.status_message = ev.message.clone();
            if ev.target.is_some() {
                run.target = ev.target.clone();
            }
            if ev.tool.is_some() {
                run.tool = ev.tool.clone();
            }
            run.updated_at = ev.ts;
        }
        {
            let mut q = self.recent.lock();
            if q.len() >= 100 {
                q.pop_front();
            }
            q.push_back(ev.clone());
        }
        let _ = self.tx.send(ev);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn emit_phase(
        &self,
        id: &str,
        parent_id: Option<&str>,
        agent: Option<&str>,
        phase: &str,
        message: impl Into<String>,
        target: Option<&str>,
        tool: Option<&str>,
    ) {
        self.emit_with_meta(id, parent_id, agent, phase, message, target, tool, None);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn emit_with_meta(
        &self,
        id: &str,
        parent_id: Option<&str>,
        agent: Option<&str>,
        phase: &str,
        message: impl Into<String>,
        target: Option<&str>,
        tool: Option<&str>,
        meta: Option<ActivityMeta>,
    ) {
        self.emit(ActivityEvent {
            id: id.to_string(),
            parent_id: parent_id.map(String::from),
            agent: agent.map(String::from),
            phase: phase.to_string(),
            message: message.into(),
            target: target.map(String::from),
            tool: tool.map(String::from),
            meta,
            ts: now(),
        });
    }

    pub fn create_run(&self, id: String, parent_session: Option<String>, role: String, model: String) -> SubagentRun {
        let ts = now();
        let run = SubagentRun {
            id: id.clone(),
            parent_session: parent_session.clone(),
            role: role.clone(),
            model,
            phase: "queued".into(),
            status_message: format!("Queued subagent '{role}'"),
            target: None,
            tool: None,
            created_at: ts,
            updated_at: ts,
            output: None,
            error: None,
        };
        self.runs.lock().insert(id.clone(), run.clone());
        self.emit_phase(&id, parent_session.as_deref(), Some(&role), "queued", format!("Queued subagent '{role}'"), None, None);
        run
    }

    pub fn finish_run(&self, id: &str, output: Option<String>, error: Option<String>) {
        let mut runs = self.runs.lock();
        if let Some(run) = runs.get_mut(id) {
            run.updated_at = now();
            if let Some(err) = error {
                run.phase = "failed".into();
                run.status_message = err.clone();
                run.error = Some(err);
            } else {
                run.phase = "completed".into();
                run.status_message = "Completed".into();
                run.output = output;
            }
        }
    }

    pub fn list_runs(&self) -> Vec<SubagentRun> {
        let mut out: Vec<SubagentRun> = self.runs.lock().values().cloned().collect();
        out.sort_by(|a, b| b.created_at.partial_cmp(&a.created_at).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    pub fn get_run(&self, id: &str) -> Option<SubagentRun> {
        self.runs.lock().get(id).cloned()
    }

    pub fn delete_run(&self, id: &str) -> bool {
        self.runs.lock().remove(id).is_some()
    }
}

fn line_count(s: &str) -> usize {
    if s.is_empty() { 0 } else { s.lines().count().max(1) }
}

/// Extracts structured IDE-style metadata (`action`, `file`, `lines`, `+added -removed`, `command`)
/// from a tool call name and its JSON arguments.
pub fn build_tool_activity_meta(tool_name: &str, args: &Value) -> (ActivityMeta, String) {
    let short_name = crate::mcp::parse_tool_name(tool_name).map(|(_, t)| t).unwrap_or(tool_name);
    let lower = short_name.to_ascii_lowercase();

    let action = if lower.contains("read") || lower.contains("view") || lower.contains("get_file") || lower.contains("cat") {
        "Analyzed"
    } else if lower.contains("write") || lower.contains("create") {
        "Created"
    } else if lower.contains("edit") || lower.contains("replace") || lower.contains("patch") || lower.contains("modify") {
        "Edited"
    } else if lower.contains("delete") || lower.contains("remove") {
        "Deleted"
    } else if lower.contains("search") || lower.contains("grep") || lower.contains("find") || lower.contains("list") {
        "Searched"
    } else if lower.contains("run") || lower.contains("exec") || lower.contains("bash") || lower.contains("shell") {
        "Ran"
    } else {
        "Called"
    };

    let file = ["path", "file_path", "file", "Path", "target_file", "filename"]
        .iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string());

    let start = args["start_line"].as_u64().or_else(|| args["StartLine"].as_u64()).or_else(|| args["offset"].as_u64());
    let end = args["end_line"].as_u64().or_else(|| args["EndLine"].as_u64());
    let lines = match (start, end) {
        (Some(s), Some(e)) => Some(format!("L{s}-{e}")),
        (Some(s), None) => Some(format!("L{s}")),
        _ => None,
    };

    let (added, removed) = if action == "Created" {
        let c = args["content"].as_str().or_else(|| args["Content"].as_str()).unwrap_or("");
        (Some(line_count(c)), Some(0))
    } else if action == "Edited" {
        if let Some(edits) = args["edits"].as_array() {
            let mut a = 0;
            let mut r = 0;
            for e in edits {
                r += line_count(e["oldText"].as_str().or_else(|| e["old_string"].as_str()).unwrap_or(""));
                a += line_count(e["newText"].as_str().or_else(|| e["new_string"].as_str()).unwrap_or(""));
            }
            (Some(a), Some(r))
        } else {
            let old = args["old_string"].as_str().or_else(|| args["Target"].as_str()).or_else(|| args["oldText"].as_str()).unwrap_or("");
            let new = args["new_string"].as_str().or_else(|| args["Replacement"].as_str()).or_else(|| args["newText"].as_str()).unwrap_or("");
            (Some(line_count(new)), Some(line_count(old)))
        }
    } else {
        (None, None)
    };

    let command = ["command", "CommandLine", "cmd", "query"]
        .iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .map(String::from);

    let mut msg = action.to_string();
    if let Some(ref f) = file {
        msg.push(' ');
        msg.push_str(f);
    } else if let Some(ref cmd) = command {
        msg.push(' ');
        msg.push_str(&cmd.chars().take(60).collect::<String>());
    } else {
        msg.push_str(&format!(" `{short_name}`"));
    }
    if let Some(ref l) = lines {
        msg.push_str(&format!(" #{l}"));
    }
    if let (Some(a), Some(r)) = (added, removed) {
        msg.push_str(&format!(" +{a} -{r}"));
    }

    let meta = ActivityMeta {
        action: Some(action.to_string()),
        file,
        lines,
        added,
        removed,
        command,
        duration_ms: None,
    };
    (meta, msg)
}
