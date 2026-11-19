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
    /// Run or request that spawned this one (multi-agent trees).
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Virtual key the run was spawned under; its budget and limits apply.
    #[serde(default)]
    pub key_id: Option<String>,
}

impl SubagentRun {
    pub fn is_finished(&self) -> bool {
        matches!(self.phase.as_str(), "completed" | "failed" | "cancelled")
    }
}

/// Finished runs kept in memory; older ones live only in the store.
const MAX_KEPT_RUNS: usize = 200;

#[derive(Clone)]
pub struct ActivityHub {
    tx: broadcast::Sender<ActivityEvent>,
    recent: Arc<Mutex<VecDeque<ActivityEvent>>>,
    runs: Arc<Mutex<HashMap<String, SubagentRun>>>,
    tasks: Arc<Mutex<HashMap<String, tokio::task::AbortHandle>>>,
    /// Caps concurrent subagents so one prompt cannot fan out into hundreds of upstream calls.
    pub slots: Arc<tokio::sync::Semaphore>,
    done: Arc<tokio::sync::Notify>,
    approvals: Arc<Mutex<HashMap<String, PendingSlot>>>,
    /// Notes posted inside one run, keyed by the root run id. Shared by the lead and its workers only.
    notes: Arc<Mutex<TeamLog>>,
}

pub const MAX_CONCURRENT_SUBAGENTS: usize = 8;

/// A tool call waiting for a person to approve or deny it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub id: String,
    /// The request or subagent run that wants to call the tool.
    pub run_id: String,
    pub agent: Option<String>,
    pub tool: String,
    pub arguments: Value,
    pub created_at: f64,
    pub expires_at: f64,
}

type PendingSlot = (PendingApproval, tokio::sync::oneshot::Sender<bool>);
/// Who posted a line on a run's team thread, and the text.
type TeamNote = (String, String);
type TeamLog = HashMap<String, Vec<TeamNote>>;

/// How long a tool call waits for approval before it counts as denied.
pub const APPROVAL_TIMEOUT_SECS: u64 = 300;

impl Default for ActivityHub {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            tx,
            recent: Arc::new(Mutex::new(VecDeque::with_capacity(100))),
            runs: Default::default(),
            tasks: Default::default(),
            slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SUBAGENTS)),
            done: Default::default(),
            approvals: Default::default(),
            notes: Default::default(),
        }
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
            // A run is over only when finish_run/cancel_run says so. The per-request
            // `completed`/`failed` events also fire for each turn inside a run (tool loops,
            // delegate), and must not end the run early.
            let terminal = matches!(ev.phase.as_str(), "completed" | "failed" | "cancelled");
            if !terminal && !run.is_finished() {
                run.phase = ev.phase.clone();
            }
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
        self.create_run_with(id, parent_session, role, model, None, None)
    }

    pub fn create_run_with(
        &self,
        id: String,
        parent_session: Option<String>,
        role: String,
        model: String,
        parent_id: Option<String>,
        key_id: Option<String>,
    ) -> SubagentRun {
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
            parent_id: parent_id.clone(),
            key_id,
        };
        {
            let mut runs = self.runs.lock();
            runs.insert(id.clone(), run.clone());
            prune_finished(&mut runs);
        }
        let parent = parent_id.as_deref().or(parent_session.as_deref());
        self.emit_phase(&id, parent, Some(&role), "queued", format!("Queued subagent '{role}'"), None, None);
        run
    }

    /// Parks a tool call until someone approves or denies it, or it times out (= denied).
    /// Emits `awaiting_approval` with the approval id so a console can show it.
    pub async fn request_approval(&self, run_id: &str, parent: Option<&str>, agent: Option<&str>, tool: &str, arguments: Value) -> bool {
        self.request_approval_for(run_id, parent, agent, tool, arguments, std::time::Duration::from_secs(APPROVAL_TIMEOUT_SECS)).await
    }

    pub async fn request_approval_for(
        &self,
        run_id: &str,
        parent: Option<&str>,
        agent: Option<&str>,
        tool: &str,
        arguments: Value,
        timeout: std::time::Duration,
    ) -> bool {
        let id = format!("apr_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let created_at = now();
        let pending = PendingApproval {
            id: id.clone(),
            run_id: run_id.to_string(),
            agent: agent.map(String::from),
            tool: tool.to_string(),
            arguments,
            created_at,
            expires_at: created_at + timeout.as_secs_f64(),
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.approvals.lock().insert(id.clone(), (pending, tx));
        self.emit_with_meta(
            run_id,
            parent,
            agent,
            "awaiting_approval",
            format!("Waiting for approval to run `{tool}` ({id})"),
            None,
            Some(tool),
            None,
        );
        let approved = matches!(tokio::time::timeout(timeout, rx).await, Ok(Ok(true)));
        self.approvals.lock().remove(&id);
        let (phase, msg) =
            if approved { ("approved", format!("Approved `{tool}`")) } else { ("denied", format!("Did not run `{tool}`: not approved")) };
        self.emit_phase(run_id, parent, agent, phase, msg, None, Some(tool));
        approved
    }

    pub fn list_approvals(&self) -> Vec<PendingApproval> {
        let mut v: Vec<PendingApproval> = self.approvals.lock().values().map(|(p, _)| p.clone()).collect();
        v.sort_by(|a, b| a.created_at.partial_cmp(&b.created_at).unwrap_or(std::cmp::Ordering::Equal));
        v
    }

    /// Answers a pending approval; false if it is unknown or already answered or expired.
    pub fn resolve_approval(&self, id: &str, approve: bool) -> bool {
        match self.approvals.lock().remove(id) {
            Some((_, tx)) => tx.send(approve).is_ok(),
            None => false,
        }
    }

    pub fn attach_task(&self, id: &str, handle: tokio::task::AbortHandle) {
        self.tasks.lock().insert(id.to_string(), handle);
    }

    /// Stops this run and every run it started. Returns the ids that were still going.
    /// Leaves go first, so aborting a parent cannot leave its children running.
    pub fn cancel_tree(&self, id: &str) -> Vec<String> {
        let ids = {
            let runs = self.runs.lock();
            let mut want = vec![id.to_string()];
            let mut i = 0;
            while i < want.len() && want.len() < 64 {
                let parent = want[i].clone();
                for r in runs.values() {
                    if r.parent_id.as_deref() == Some(parent.as_str()) && !want.iter().any(|w| w == &r.id) {
                        want.push(r.id.clone());
                    }
                }
                i += 1;
            }
            want
        };
        let mut stopped = Vec::new();
        for id in ids.into_iter().rev() {
            if self.cancel_one(&id) {
                stopped.push(id);
            }
        }
        stopped
    }

    /// Stops a running subagent and the runs under it. Returns false if none were running.
    pub fn cancel_run(&self, id: &str) -> bool {
        !self.cancel_tree(id).is_empty()
    }

    fn cancel_one(&self, id: &str) -> bool {
        let Some(h) = self.tasks.lock().remove(id) else { return false };
        h.abort();
        let Some(role) = self.mark_cancelled_inner(id) else { return true };
        self.emit_phase(id, None, Some(&role), "cancelled", "Cancelled", None, None);
        self.done.notify_waiters();
        true
    }

    /// Marks a run cancelled even when it has no task of its own (a client request that delegated).
    /// Returns it only when this call changed the phase.
    pub fn mark_cancelled(&self, id: &str) -> Option<SubagentRun> {
        let role = self.mark_cancelled_inner(id)?;
        let run = self.get_run(id)?;
        if run.parent_id.is_none() {
            self.notes.lock().remove(&run.id);
        }
        self.emit_phase(id, None, Some(&role), "cancelled", "Cancelled", None, None);
        self.done.notify_waiters();
        Some(run)
    }

    fn mark_cancelled_inner(&self, id: &str) -> Option<String> {
        let mut runs = self.runs.lock();
        let run = runs.get_mut(id)?;
        if run.is_finished() {
            return None;
        }
        run.phase = "cancelled".into();
        run.status_message = "Cancelled".into();
        run.updated_at = now();
        Some(run.role.clone())
    }

    /// The root of this run: the client request, or this id when nothing started it.
    pub fn team_root(&self, id: &str) -> String {
        let runs = self.runs.lock();
        let mut current = id.to_string();
        for _ in 0..8 {
            match runs.get(&current).and_then(|r| r.parent_id.clone()) {
                Some(parent) => current = parent,
                None => break,
            }
        }
        current
    }

    /// Posts a note on a run's thread and returns the thread. An empty message only reads.
    /// Other runs cannot see it. The thread is dropped when the root run finishes.
    pub fn team_post(&self, root: &str, from: &str, message: &str) -> String {
        const MAX_NOTES: usize = 40;
        const MAX_CHARS: usize = 500;
        let text: String = message.trim().chars().take(MAX_CHARS).collect();
        let mut notes = self.notes.lock();
        let log = notes.entry(root.to_string()).or_default();
        if !text.is_empty() {
            log.push((from.trim().to_string(), text));
            if log.len() > MAX_NOTES {
                let drop_n = log.len() - MAX_NOTES;
                log.drain(0..drop_n);
            }
        }
        if log.is_empty() {
            "(no notes yet)".into()
        } else {
            log.iter().map(|(who, text)| format!("[{who}] {text}")).collect::<Vec<_>>().join("\n")
        }
    }

    /// Waits until the run finishes or `timeout` passes; returns its latest state.
    pub async fn wait_run(&self, id: &str, timeout: std::time::Duration) -> Option<SubagentRun> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.done.notified();
            let run = self.get_run(id)?;
            if run.is_finished() {
                return Some(run);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.get_run(id);
            }
        }
    }

    pub fn finish_run(&self, id: &str, output: Option<String>, error: Option<String>) -> Option<SubagentRun> {
        self.tasks.lock().remove(id);
        let finished = self.finish_inner(id, output, error)?;
        if finished.parent_id.is_none() {
            self.notes.lock().remove(&finished.id);
        }
        self.done.notify_waiters();
        Some(finished)
    }

    fn finish_inner(&self, id: &str, output: Option<String>, error: Option<String>) -> Option<SubagentRun> {
        let mut runs = self.runs.lock();
        let run = runs.get_mut(id)?;
        // Cancel wins. A task that finishes after it was stopped must not flip the run back.
        if run.is_finished() {
            return None;
        }
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
        Some(run.clone())
    }

    /// Restores finished runs saved in the store so history survives restarts.
    pub fn restore_runs(&self, saved: Vec<SubagentRun>) {
        let mut runs = self.runs.lock();
        for r in saved {
            runs.entry(r.id.clone()).or_insert(r);
        }
        prune_finished(&mut runs);
    }

    pub fn list_runs(&self) -> Vec<SubagentRun> {
        let mut out: Vec<SubagentRun> = self.runs.lock().values().cloned().collect();
        out.sort_by(|a, b| b.created_at.partial_cmp(&a.created_at).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    /// Runs as a forest: every run whose parent is not itself a known run becomes a root (its
    /// parent is a client request or nothing), and children nest under `children`. Newest roots
    /// first, children in start order, so the tree reads like the order work was handed out.
    pub fn run_tree(&self) -> Vec<Value> {
        let runs = self.list_runs();
        let ids: std::collections::HashSet<&str> = runs.iter().map(|r| r.id.as_str()).collect();
        let mut children: HashMap<&str, Vec<&SubagentRun>> = HashMap::new();
        for r in &runs {
            if let Some(p) = r.parent_id.as_deref().filter(|p| ids.contains(p)) {
                children.entry(p).or_default().push(r);
            }
        }
        for v in children.values_mut() {
            v.sort_by(|a, b| a.created_at.partial_cmp(&b.created_at).unwrap_or(std::cmp::Ordering::Equal));
        }
        fn node(r: &SubagentRun, children: &HashMap<&str, Vec<&SubagentRun>>, depth: usize) -> Value {
            let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
            // Depth is bounded by the delegate limit; the guard only protects against bad data.
            let kids: Vec<Value> = if depth < 16 {
                children.get(r.id.as_str()).map(|c| c.iter().map(|k| node(k, children, depth + 1)).collect()).unwrap_or_default()
            } else {
                Vec::new()
            };
            v["children"] = Value::Array(kids);
            v
        }
        runs.iter().filter(|r| !r.parent_id.as_deref().is_some_and(|p| ids.contains(p))).map(|r| node(r, &children, 0)).collect()
    }

    pub fn get_run(&self, id: &str) -> Option<SubagentRun> {
        self.runs.lock().get(id).cloned()
    }

    pub fn delete_run(&self, id: &str) -> bool {
        self.cancel_run(id);
        self.runs.lock().remove(id).is_some()
    }
}

/// Drops the oldest finished runs beyond `MAX_KEPT_RUNS`; running ones are never dropped.
fn prune_finished(runs: &mut HashMap<String, SubagentRun>) {
    let mut finished: Vec<(f64, String)> = runs.values().filter(|r| r.is_finished()).map(|r| (r.updated_at, r.id.clone())).collect();
    if finished.len() <= MAX_KEPT_RUNS {
        return;
    }
    finished.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    for (_, id) in finished.into_iter().take(runs.len().saturating_sub(MAX_KEPT_RUNS)) {
        runs.remove(&id);
    }
}

fn line_count(s: &str) -> usize {
    if s.is_empty() {
        0
    } else {
        s.lines().count().max(1)
    }
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
            let new =
                args["new_string"].as_str().or_else(|| args["Replacement"].as_str()).or_else(|| args["newText"].as_str()).unwrap_or("");
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

    let meta = ActivityMeta { action: Some(action.to_string()), file, lines, added, removed, command, duration_ms: None };
    (meta, msg)
}
