use crate::store::now;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::broadcast;

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
        self.emit(ActivityEvent {
            id: id.to_string(),
            parent_id: parent_id.map(String::from),
            agent: agent.map(String::from),
            phase: phase.to_string(),
            message: message.into(),
            target: target.map(String::from),
            tool: tool.map(String::from),
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
