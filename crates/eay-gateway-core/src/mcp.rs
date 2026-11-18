use crate::store::McpServerRecord;
use reqwest::Client;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Encodes an external MCP tool as `mcp__<server>__<tool>` so multiple servers never collide.
pub fn encode_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// Splits `mcp__<server>__<tool>` back into `(server, tool)`.
pub fn parse_tool_name(full_name: &str) -> Option<(&str, &str)> {
    let rest = full_name.strip_prefix("mcp__")?;
    rest.split_once("__")
}

async fn send_line(stdin: &mut tokio::process::ChildStdin, msg: Value) -> Result<(), String> {
    let mut line = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).await.map_err(|e| format!("MCP write failed: {e}"))?;
    stdin.flush().await.map_err(|e| format!("MCP flush failed: {e}"))
}

async fn read_rpc_id(reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>, target_id: i64) -> Result<Value, String> {
    while let Ok(Some(line)) = reader.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            if v.get("id").and_then(|i| i.as_i64()) == Some(target_id) {
                if let Some(err) = v.get("error") {
                    return Err(err["message"].as_str().map(String::from).unwrap_or_else(|| err.to_string()));
                }
                return Ok(v["result"].clone());
            }
        }
    }
    Err("MCP server closed stdout before replying".into())
}

async fn stdio_rpc(server: &McpServerRecord, method: &str, params: Value) -> Result<Value, String> {
    let cmd = server.command.as_deref().filter(|s| !s.trim().is_empty()).ok_or("MCP stdio server missing command")?;
    let mut child = Command::new(cmd)
        .args(&server.args)
        .envs(&server.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Failed to spawn MCP '{}': {e}", server.name))?;

    let mut stdin = child.stdin.take().ok_or("MCP stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("MCP stdout unavailable")?;
    let mut reader = BufReader::new(stdout).lines();

    let init_req = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "eay-gateway", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    send_line(&mut stdin, init_req).await?;

    tokio::time::timeout(Duration::from_secs(15), read_rpc_id(&mut reader, 1))
        .await
        .map_err(|_| format!("MCP '{}' initialize timed out", server.name))??;

    let inited = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    send_line(&mut stdin, inited).await?;

    let req = json!({ "jsonrpc": "2.0", "id": 2, "method": method, "params": params });
    send_line(&mut stdin, req).await?;

    let result = tokio::time::timeout(Duration::from_secs(20), read_rpc_id(&mut reader, 2))
        .await
        .map_err(|_| format!("MCP '{}' {method} timed out", server.name))??;

    let _ = child.kill().await;
    Ok(result)
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

async fn http_rpc(http: &Client, server: &McpServerRecord, method: &str, params: Value) -> Result<Value, String> {
    let url = server.url.as_deref().filter(|s| !s.trim().is_empty()).ok_or("MCP http server missing url")?;

    // Streamable HTTP handshake: `initialize` may return `mcp-session-id` required for later calls.
    let init_req = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "eay-gateway", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    let mut session_id: Option<String> = None;
    if let Ok(init_resp) = apply_http_headers(http.post(url), server, None).json(&init_req).timeout(Duration::from_secs(15)).send().await {
        if init_resp.status().is_success() {
            session_id = init_resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(String::from);
            let inited = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
            let _ = apply_http_headers(http.post(url), server, session_id.as_deref())
                .json(&inited)
                .timeout(Duration::from_secs(10))
                .send()
                .await;
        }
    }

    let req = json!({ "jsonrpc": "2.0", "id": 2, "method": method, "params": params });
    let resp = apply_http_headers(http.post(url), server, session_id.as_deref())
        .json(&req)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("MCP '{}' HTTP error: {e}", server.name))?;
    if !resp.status().is_success() {
        let st = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(format!("MCP '{}' HTTP {st}: {txt}", server.name));
    }
    let body = resp.text().await.map_err(|e| e.to_string())?;
    let parsed: Value = if body.trim_start().starts_with("data:") || body.contains("\ndata:") {
        body.lines()
            .filter_map(|l| l.trim().strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
            .find(|v| v.get("result").is_some() || v.get("error").is_some())
            .ok_or_else(|| "MCP SSE response contained no JSON-RPC result".to_string())?
    } else {
        serde_json::from_str(&body).map_err(|e| format!("Invalid MCP JSON response: {e}"))?
    };
    if let Some(err) = parsed.get("error") {
        return Err(err["message"].as_str().map(String::from).unwrap_or_else(|| err.to_string()));
    }
    Ok(parsed["result"].clone())
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
    let res = rpc(http, server, "tools/list", json!({})).await?;
    let tools = res["tools"].as_array().cloned().unwrap_or_default();
    Ok(tools
        .into_iter()
        .filter_map(|t| {
            let name = t["name"].as_str()?;
            let schema =
                if t["inputSchema"].is_object() { t["inputSchema"].clone() } else { json!({ "type": "object", "properties": {} }) };
            Some(json!({
                "type": "function",
                "function": {
                    "name": encode_tool_name(&server.name, name),
                    "description": t["description"].as_str().unwrap_or(""),
                    "parameters": schema,
                }
            }))
        })
        .collect())
}

/// Executes a tool on an external MCP server and returns its text output.
pub async fn call_tool(http: &Client, server: &McpServerRecord, tool_name: &str, arguments: Value) -> Result<String, String> {
    let res = rpc(http, server, "tools/call", json!({ "name": tool_name, "arguments": arguments })).await?;
    let mut parts = Vec::new();
    for item in res["content"].as_array().into_iter().flatten() {
        if let Some(t) = item["text"].as_str() {
            parts.push(t.to_string());
        } else {
            parts.push(item.to_string());
        }
    }
    let out = if parts.is_empty() { res.to_string() } else { parts.join("\n") };
    if res["isError"].as_bool() == Some(true) {
        Err(out)
    } else {
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_parses_mcp_tool_names() {
        let encoded = encode_tool_name("filesystem", "read_file");
        assert_eq!(encoded, "mcp__filesystem__read_file");
        assert_eq!(parse_tool_name(&encoded), Some(("filesystem", "read_file")));
        assert_eq!(parse_tool_name("regular_tool"), None);
    }
}
