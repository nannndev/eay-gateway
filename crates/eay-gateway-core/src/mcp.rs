use crate::store::McpServerRecord;
use reqwest::Client;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// OpenAI and Anthropic reject tool names longer than this or outside `[A-Za-z0-9_-]`.
const MAX_TOOL_NAME: usize = 64;

/// Encodes an external MCP tool as `mcp__<server>__<tool>` so multiple servers never collide.
/// Names that would be too long or contain other characters are shortened to a stable
/// `mcp__<server>__<tool-prefix>_<hash>`; resolve those with [`McpToolIndex`], not [`parse_tool_name`].
pub fn encode_tool_name(server: &str, tool: &str) -> String {
    let full = format!("mcp__{server}__{tool}");
    let valid = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if full.len() <= MAX_TOOL_NAME && valid(&full) {
        return full;
    }
    use sha2::Digest;
    let hash: String = sha2::Sha256::digest(full.as_bytes()).iter().take(4).map(|b| format!("{b:02x}")).collect();
    let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect::<String>();
    let head = format!("mcp__{}__{}", clean(server), clean(tool));
    // 9 = "_" + 8 hex chars
    format!("{}_{hash}", head.chars().take(MAX_TOOL_NAME - 9).collect::<String>())
}

/// Maps tool names as shown to the model back to `(server, tool)`, including shortened ones.
#[derive(Debug, Default, Clone)]
pub struct McpToolIndex(std::collections::HashMap<String, (String, String)>);

impl McpToolIndex {
    pub fn insert(&mut self, server: &str, tool: &str) -> String {
        let name = encode_tool_name(server, tool);
        self.0.insert(name.clone(), (server.to_string(), tool.to_string()));
        name
    }

    pub fn resolve(&self, shown: &str) -> Option<(&str, &str)> {
        self.0.get(shown).map(|(s, t)| (s.as_str(), t.as_str()))
    }

    pub fn extend(&mut self, other: McpToolIndex) {
        self.0.extend(other.0);
    }
}

/// Splits `mcp__<server>__<tool>` back into `(server, tool)`.
pub fn parse_tool_name(full_name: &str) -> Option<(&str, &str)> {
    let rest = full_name.strip_prefix("mcp__")?;
    rest.split_once("__")
}

/// `Command` does not go through a shell, so `~/Desktop` would reach the server literally.
fn expand_home(s: &str) -> String {
    let home = || std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok();
    match (s.strip_prefix("~/"), s == "~") {
        (Some(rest), _) => home().map(|h| format!("{h}/{rest}")).unwrap_or_else(|| s.into()),
        (None, true) => home().unwrap_or_else(|| s.into()),
        _ => s.into(),
    }
}

/// One live stdio MCP server process, reused across calls so stateful servers (browsers, DB
/// sessions) keep their state and nothing pays a cold start per tool call.
struct StdioConn {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    next_id: i64,
    last_used: std::time::Instant,
    /// Last lines the server wrote to stderr, shown when it fails (missing npm package, bad token, …).
    stderr_tail: std::sync::Arc<parking_lot::Mutex<std::collections::VecDeque<String>>>,
}

/// Why a call failed: `Dead` means the process or pipe is gone, so a fresh spawn may succeed.
enum CallErr {
    Dead(String),
    Other(String),
}

type Slot = std::sync::Arc<tokio::sync::Mutex<Option<StdioConn>>>;

/// Live servers keyed by `name@updated_at`, so editing a server starts a new process.
fn pool() -> &'static parking_lot::Mutex<std::collections::HashMap<String, Slot>> {
    static POOL: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashMap<String, Slot>>> = std::sync::OnceLock::new();
    POOL.get_or_init(Default::default)
}

const IDLE_TTL: Duration = Duration::from_secs(600);

impl StdioConn {
    async fn spawn(server: &McpServerRecord) -> Result<Self, String> {
        let cmd = server.command.as_deref().filter(|s| !s.trim().is_empty()).ok_or("MCP stdio server missing command")?;
        let mut child = Command::new(expand_home(cmd.trim()))
            .args(server.args.iter().map(|a| expand_home(a)))
            .envs(&server.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Failed to spawn MCP '{}': {e}", server.name))?;
        let stdin = child.stdin.take().ok_or("MCP stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("MCP stdout unavailable")?;
        // Drain stderr so a chatty server never blocks on a full pipe; keep the tail for error messages.
        let stderr_tail: std::sync::Arc<parking_lot::Mutex<std::collections::VecDeque<String>>> = Default::default();
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            let name = server.name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!("[mcp:{name}] {line}");
                    let mut t = tail.lock();
                    if t.len() >= 20 {
                        t.pop_front();
                    }
                    t.push_back(line.chars().take(400).collect());
                }
            });
        }
        let mut conn =
            Self { child, stdin, reader: BufReader::new(stdout).lines(), next_id: 1, last_used: std::time::Instant::now(), stderr_tail };
        let init = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "eay-gateway", "version": env!("CARGO_PKG_VERSION") }
        });
        if let Err(CallErr::Dead(m) | CallErr::Other(m)) = conn.request("initialize", init, Duration::from_secs(15)).await {
            return Err(format!("MCP '{}' initialize failed: {m}{}", server.name, conn.stderr_note().await));
        }
        conn.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await.map_err(|e| match e {
            CallErr::Dead(m) | CallErr::Other(m) => m,
        })?;
        Ok(conn)
    }

    /// ` (stderr: …)` with the server's last stderr lines, or empty. Waits briefly so a crashing
    /// process has time to print why.
    async fn stderr_note(&self) -> String {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let t = self.stderr_tail.lock();
        let lines: Vec<&str> = t.iter().rev().take(5).map(String::as_str).collect::<Vec<_>>().into_iter().rev().collect();
        if lines.is_empty() {
            String::new()
        } else {
            format!(" (stderr: {})", lines.join(" | "))
        }
    }

    async fn send(&mut self, msg: Value) -> Result<(), CallErr> {
        let mut line = serde_json::to_string(&msg).map_err(|e| CallErr::Other(e.to_string()))?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.map_err(|e| CallErr::Dead(format!("MCP write failed: {e}")))?;
        self.stdin.flush().await.map_err(|e| CallErr::Dead(format!("MCP flush failed: {e}")))
    }

    async fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, CallErr> {
        let id = self.next_id;
        self.next_id += 1;
        self.last_used = std::time::Instant::now();
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        let read = async {
            loop {
                let line = match self.reader.next_line().await {
                    Ok(Some(l)) => l,
                    Ok(None) => return Err(CallErr::Dead("MCP server closed stdout before replying".into())),
                    Err(e) => return Err(CallErr::Dead(format!("MCP read failed: {e}"))),
                };
                let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
                if v.get("id").and_then(Value::as_i64) != Some(id) {
                    continue; // notifications, logs, or server-initiated requests
                }
                if let Some(err) = v.get("error") {
                    return Err(CallErr::Other(err["message"].as_str().map(String::from).unwrap_or_else(|| err.to_string())));
                }
                return Ok(v["result"].clone());
            }
        };
        // A timed-out request leaves an unread reply on the pipe; treat the connection as unusable.
        tokio::time::timeout(timeout, read).await.map_err(|_| CallErr::Dead(format!("{method} timed out after {}s", timeout.as_secs())))?
    }
}

/// Kills idle processes and ones whose server record changed since they started.
fn sweep(current_key: &str, name: &str) {
    let mut pool = pool().lock();
    pool.retain(|k, slot| {
        let stale_version = k != current_key && k.split_once('@').map(|(n, _)| n) == Some(name);
        let idle = slot.try_lock().map(|g| g.as_ref().is_none_or(|c| c.last_used.elapsed() > IDLE_TTL)).unwrap_or(false);
        !(stale_version || idle && k != current_key)
    });
}

/// Stops every live process of a server; call after it is deleted, disabled or edited.
pub fn shutdown_server(name: &str) {
    let other = |k: &String| k.split_once('@').map(|(n, _)| n) != Some(name);
    pool().lock().retain(|k, _| other(k));
    http_sessions().lock().retain(|k, _| other(k));
}

async fn stdio_rpc(server: &McpServerRecord, method: &str, params: Value) -> Result<Value, String> {
    let key = format!("{}@{}", server.name, server.updated_at);
    sweep(&key, &server.name);
    let slot = pool().lock().entry(key).or_default().clone();
    let mut guard = slot.lock().await; // one call at a time per server; stdio MCP is a single pipe
                                       // Tool calls can run builds or browsers; listing should be quick.
    let timeout = if method == "tools/call" { Duration::from_secs(120) } else { Duration::from_secs(20) };
    for attempt in 0..2 {
        let reused = guard.as_ref().is_some_and(|c| c.last_used.elapsed() < IDLE_TTL);
        if !reused {
            *guard = Some(StdioConn::spawn(server).await?);
        }
        let Some(conn) = guard.as_mut() else { break };
        match conn.request(method, params.clone(), timeout).await {
            Ok(v) => return Ok(v),
            Err(CallErr::Other(m)) => return Err(m),
            Err(CallErr::Dead(m)) => {
                let note = match guard.take() {
                    Some(mut c) => {
                        let note = c.stderr_note().await;
                        let _ = c.child.kill().await;
                        note
                    }
                    None => String::new(),
                };
                // Only a reused process may have died between calls; a fresh one failing is a real error.
                if !reused || attempt == 1 {
                    return Err(format!("MCP '{}' {m}{note}", server.name));
                }
            }
        }
    }
    Err(format!("MCP '{}' is unavailable", server.name))
}

fn apply_http_headers(mut req: reqwest::RequestBuilder, server: &McpServerRecord, session_id: Option<&str>) -> reqwest::RequestBuilder {
    req = req.header("Accept", "application/json, text/event-stream");
    for (k, v) in &server.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(sid) = session_id {
        req = req.header("mcp-session-id", sid);
    }
    req
}

/// `mcp-session-id` per HTTP server (`name@updated_at`); `None` = initialized, server is stateless.
fn http_sessions() -> &'static parking_lot::Mutex<std::collections::HashMap<String, Option<String>>> {
    static S: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashMap<String, Option<String>>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// Parses a JSON-RPC reply that arrived as plain JSON or as an SSE body.
fn parse_rpc_body(body: &str) -> Result<Value, String> {
    if body.trim_start().starts_with("data:") || body.contains("\ndata:") || body.contains("\nevent:") || body.starts_with("event:") {
        body.lines()
            .filter_map(|l| l.trim().strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
            .find(|v| v.get("result").is_some() || v.get("error").is_some())
            .ok_or_else(|| "MCP SSE response contained no JSON-RPC result".to_string())
    } else {
        serde_json::from_str(body).map_err(|e| format!("Invalid MCP JSON response: {e}"))
    }
}

/// Runs the Streamable HTTP handshake and returns the session id the server assigned, if any.
async fn http_initialize(http: &Client, url: &str, server: &McpServerRecord) -> Result<Option<String>, String> {
    let init_req = json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "eay-gateway", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    let resp = apply_http_headers(http.post(url), server, None)
        .json(&init_req)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("MCP '{}' HTTP error: {e}", server.name))?;
    // Some servers skip the handshake entirely; treat a refusal as "no session" and let the call decide.
    if !resp.status().is_success() {
        return Ok(None);
    }
    let sid = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(String::from);
    let inited = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let _ = apply_http_headers(http.post(url), server, sid.as_deref()).json(&inited).timeout(Duration::from_secs(10)).send().await;
    Ok(sid)
}

async fn http_rpc(http: &Client, server: &McpServerRecord, method: &str, params: Value) -> Result<Value, String> {
    let url = server.url.as_deref().filter(|s| !s.trim().is_empty()).ok_or("MCP http server missing url")?;
    let key = format!("{}@{}", server.name, server.updated_at);
    let timeout = if method == "tools/call" { Duration::from_secs(120) } else { Duration::from_secs(20) };
    for attempt in 0..2 {
        let cached = http_sessions().lock().get(&key).cloned();
        let session_id = match cached {
            Some(sid) => sid,
            None => {
                let sid = http_initialize(http, url, server).await?;
                http_sessions().lock().insert(key.clone(), sid.clone());
                sid
            }
        };
        let req = json!({ "jsonrpc": "2.0", "id": rand::random::<u32>(), "method": method, "params": params });
        let resp = apply_http_headers(http.post(url), server, session_id.as_deref())
            .json(&req)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| format!("MCP '{}' HTTP error: {e}", server.name))?;
        let st = resp.status();
        // 404 on a session means it expired server-side: start a new one once.
        if st == reqwest::StatusCode::NOT_FOUND && session_id.is_some() && attempt == 0 {
            http_sessions().lock().remove(&key);
            continue;
        }
        if !st.is_success() {
            let txt = resp.text().await.unwrap_or_default();
            return Err(format!("MCP '{}' HTTP {st}: {}", server.name, txt.chars().take(500).collect::<String>()));
        }
        let body = resp.text().await.map_err(|e| e.to_string())?;
        let parsed = parse_rpc_body(&body)?;
        if let Some(err) = parsed.get("error") {
            return Err(err["message"].as_str().map(String::from).unwrap_or_else(|| err.to_string()));
        }
        return Ok(parsed["result"].clone());
    }
    Err(format!("MCP '{}' session could not be re-established", server.name))
}

async fn rpc(http: &Client, server: &McpServerRecord, method: &str, params: Value) -> Result<Value, String> {
    if server.transport == "http" {
        http_rpc(http, server, method, params).await
    } else {
        stdio_rpc(server, method, params).await
    }
}

/// Fetches tools from an external MCP server and formats them as OpenAI `tools[]` definitions.
pub async fn list_tools_openai(http: &Client, server: &McpServerRecord) -> Result<Vec<Value>, String> {
    list_tools_indexed(http, server).await.map(|(tools, _)| tools)
}

/// Like [`list_tools_openai`], plus the index that maps each shown name back to the real tool.
pub async fn list_tools_indexed(http: &Client, server: &McpServerRecord) -> Result<(Vec<Value>, McpToolIndex), String> {
    let res = rpc(http, server, "tools/list", json!({})).await?;
    let mut index = McpToolIndex::default();
    let tools = res["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let name = t["name"].as_str()?;
            let schema =
                if t["inputSchema"].is_object() { t["inputSchema"].clone() } else { json!({ "type": "object", "properties": {} }) };
            Some(json!({
                "type": "function",
                "function": {
                    "name": index.insert(&server.name, name),
                    "description": t["description"].as_str().unwrap_or(""),
                    "parameters": schema,
                }
            }))
        })
        .collect();
    Ok((tools, index))
}

/// A tool's result: its text, plus any images as `data:` URLs the model can look at.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ToolOutput {
    pub text: String,
    pub images: Vec<String>,
}

/// Executes a tool on an external MCP server and returns its text output (images described in text).
pub async fn call_tool(http: &Client, server: &McpServerRecord, tool_name: &str, arguments: Value) -> Result<String, String> {
    call_tool_rich(http, server, tool_name, arguments).await.map(|o| o.text)
}

/// Like [`call_tool`], but keeps image results as images instead of dumping base64 into the text.
pub async fn call_tool_rich(http: &Client, server: &McpServerRecord, tool_name: &str, arguments: Value) -> Result<ToolOutput, String> {
    let res = rpc(http, server, "tools/call", json!({ "name": tool_name, "arguments": arguments })).await?;
    let out = tool_output(&res);
    if res["isError"].as_bool() == Some(true) {
        Err(out.text)
    } else {
        Ok(out)
    }
}

/// Splits MCP `content[]` into text and images; audio and binary resources become short notes.
fn tool_output(res: &Value) -> ToolOutput {
    let mut out = ToolOutput::default();
    let mut parts: Vec<String> = Vec::new();
    for item in res["content"].as_array().into_iter().flatten() {
        match item["type"].as_str().unwrap_or("") {
            "image" => match (item["data"].as_str(), item["mimeType"].as_str()) {
                (Some(data), Some(mime)) => {
                    out.images.push(format!("data:{mime};base64,{data}"));
                    parts.push(format!("[image {} attached]", out.images.len()));
                }
                _ => parts.push("[image without data]".into()),
            },
            "audio" => parts.push(format!("[audio {} omitted]", item["mimeType"].as_str().unwrap_or("?"))),
            "resource" => {
                let r = &item["resource"];
                match r["text"].as_str() {
                    Some(t) => parts.push(t.to_string()),
                    None => parts.push(format!(
                        "[resource {} ({})]",
                        r["uri"].as_str().unwrap_or("?"),
                        r["mimeType"].as_str().unwrap_or("binary")
                    )),
                }
            }
            "resource_link" => parts.push(format!("[resource {}]", item["uri"].as_str().unwrap_or("?"))),
            _ => match item["text"].as_str() {
                Some(t) => parts.push(t.to_string()),
                None => parts.push(item.to_string()),
            },
        }
    }
    out.text = if parts.is_empty() {
        res.get("structuredContent").map(Value::to_string).unwrap_or_else(|| res.to_string())
    } else {
        parts.join("\n")
    };
    out
}

/// One server parsed from a client config file (Claude Desktop, Cursor, Claude Code, VS Code).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct McpImport {
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: std::collections::HashMap<String, String>,
    pub url: Option<String>,
    pub headers: std::collections::HashMap<String, String>,
}

/// Reads `{"mcpServers": {...}}` (Claude/Cursor), `{"servers": {...}}` (VS Code) or a bare map.
/// Names are sanitized to the store's `[A-Za-z0-9_-]` rule; entries without command or url are skipped.
pub fn parse_client_config(cfg: &Value) -> Vec<McpImport> {
    let servers = cfg.get("mcpServers").or_else(|| cfg.get("servers")).unwrap_or(cfg);
    let str_map = |v: &Value| -> std::collections::HashMap<String, String> {
        v.as_object().map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect()).unwrap_or_default()
    };
    let Some(obj) = servers.as_object() else { return Vec::new() };
    obj.iter()
        .filter_map(|(name, s)| {
            let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
            let url = s.get("url").or_else(|| s.get("serverUrl")).and_then(Value::as_str).map(String::from);
            let command = s.get("command").and_then(Value::as_str).map(String::from);
            if url.is_none() && command.is_none() {
                return None;
            }
            Some(McpImport {
                name,
                transport: if url.is_some() { "http" } else { "stdio" }.into(),
                args: s["args"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
                env: str_map(&s["env"]),
                headers: str_map(&s["headers"]),
                command: if url.is_some() { None } else { command },
                url,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_desktop_and_vscode_configs() {
        let cfg = json!({ "mcpServers": {
            "file system": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "~/Desktop"], "env": { "A": "1" } },
            "linear": { "type": "http", "url": "https://mcp.linear.app/mcp", "headers": { "Authorization": "Bearer x" } },
            "broken": { "disabled": true }
        }});
        let mut got = parse_client_config(&cfg);
        got.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "file-system");
        assert_eq!(got[0].transport, "stdio");
        assert_eq!(got[0].args[2], "~/Desktop");
        assert_eq!(got[0].env["A"], "1");
        assert_eq!(got[1].transport, "http");
        assert_eq!(got[1].headers["Authorization"], "Bearer x");
        assert_eq!(parse_client_config(&json!({ "servers": { "x": { "command": "uvx" } } }))[0].command.as_deref(), Some("uvx"));
    }

    /// A stdio MCP server in sh that stays alive: logs its own spawn, answers any request id,
    /// and keeps a counter in memory so the test can see whether calls hit the same process.
    #[cfg(unix)]
    fn counter_server(dir: &std::path::Path, name: &str) -> McpServerRecord {
        let log = dir.join("spawns");
        let script = format!(
            r#"echo x >> {log}; n=0
while read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  n=$((n+1))
  printf '{{"jsonrpc":"2.0","id":%s,"result":{{"content":[{{"type":"text","text":"call %s"}}]}}}}\n' "$id" "$n"
done"#,
            log = log.display()
        );
        McpServerRecord {
            name: name.into(),
            transport: "stdio".into(),
            command: Some("sh".into()),
            args: vec!["-c".into(), script],
            env: Default::default(),
            url: None,
            headers: Default::default(),
            enabled: true,
            updated_at: rand::random::<u32>() as f64,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_servers_are_reused_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let srv = counter_server(dir.path(), "counter-reuse");
        let http = Client::new();
        // initialize is call 1 inside the process, so tool calls report 2 and 3 from one process.
        assert_eq!(call_tool(&http, &srv, "t", json!({})).await.unwrap(), "call 2");
        assert_eq!(call_tool(&http, &srv, "t", json!({})).await.unwrap(), "call 3");
        assert_eq!(std::fs::read_to_string(dir.path().join("spawns")).unwrap().lines().count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_changed_server_gets_a_fresh_process() {
        let dir = tempfile::tempdir().unwrap();
        let mut srv = counter_server(dir.path(), "counter-change");
        let http = Client::new();
        call_tool(&http, &srv, "t", json!({})).await.unwrap();
        srv.updated_at += 1.0;
        assert_eq!(call_tool(&http, &srv, "t", json!({})).await.unwrap(), "call 2");
        assert_eq!(std::fs::read_to_string(dir.path().join("spawns")).unwrap().lines().count(), 2);
    }

    #[test]
    fn long_or_odd_tool_names_are_shortened_and_resolvable() {
        let long_tool = "a".repeat(80);
        let mut idx = McpToolIndex::default();
        let shown = idx.insert("github", &long_tool);
        assert!(shown.len() <= MAX_TOOL_NAME, "{shown}");
        assert!(shown.starts_with("mcp__github__aaa"));
        assert_eq!(idx.resolve(&shown), Some(("github", long_tool.as_str())));
        let dotted = idx.insert("fs", "read.file");
        assert!(dotted.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "{dotted}");
        assert_eq!(idx.resolve(&dotted), Some(("fs", "read.file")));
        // Short valid names are unchanged and stable.
        assert_eq!(idx.insert("fs", "read_file"), "mcp__fs__read_file");
        assert_eq!(encode_tool_name("github", &long_tool), shown);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failing_server_reports_its_stderr() {
        let srv = McpServerRecord {
            name: "broken-stderr".into(),
            transport: "stdio".into(),
            command: Some("sh".into()),
            args: vec!["-c".into(), "echo 'npm ERR! 404 package not found' >&2; exit 1".into()],
            env: Default::default(),
            url: None,
            headers: Default::default(),
            enabled: true,
            updated_at: 1.0,
        };
        let err = list_tools_openai(&Client::new(), &srv).await.unwrap_err();
        assert!(err.contains("npm ERR! 404 package not found"), "{err}");
    }

    #[test]
    fn image_results_become_images_not_base64_text() {
        let res = json!({ "content": [
            { "type": "text", "text": "screenshot taken" },
            { "type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png" },
            { "type": "resource", "resource": { "uri": "file:///a.bin", "mimeType": "application/octet-stream", "blob": "AAAA" } }
        ]});
        let out = tool_output(&res);
        assert_eq!(out.images, ["data:image/png;base64,iVBORw0KGgo="]);
        assert!(!out.text.contains("iVBORw0KGgo"), "{}", out.text);
        assert!(out.text.contains("screenshot taken") && out.text.contains("[image 1 attached]") && out.text.contains("file:///a.bin"));
    }

    #[test]
    fn expands_a_leading_tilde_only() {
        // Same lookup as the code: HOME, or USERPROFILE on Windows.
        let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap();
        assert_eq!(expand_home("~/Desktop"), format!("{home}/Desktop"));
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("a~/b"), "a~/b");
        assert_eq!(expand_home("-y"), "-y");
    }

    #[test]
    fn encodes_and_parses_mcp_tool_names() {
        let encoded = encode_tool_name("filesystem", "read_file");
        assert_eq!(encoded, "mcp__filesystem__read_file");
        assert_eq!(parse_tool_name(&encoded), Some(("filesystem", "read_file")));
        assert_eq!(parse_tool_name("regular_tool"), None);
    }
}
