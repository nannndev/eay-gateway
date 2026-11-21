//! Shell and interactive terminal command execution strictly confined to a project's root folder.

use super::{resolve, shown, DEFAULT_CMD_TIMEOUT_SECS, MAX_CMD_OUTPUT_BYTES, MAX_CMD_TIMEOUT_SECS};
use serde_json::{json, Value};
use std::path::Path;

/// Executes an interactive terminal command confined to `root` (with stateful `cd` support).
pub async fn exec_terminal(root: &str, command: &str, cwd: Option<&str>, timeout_secs: Option<u64>) -> Result<Value, String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("command is required".into());
    }
    let root_path = Path::new(root).canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    let rel_cwd = cwd.map(str::trim).unwrap_or("");
    let work_dir = if rel_cwd.is_empty() || rel_cwd == "." {
        root_path.clone()
    } else {
        let resolved = resolve(&root_path, rel_cwd)?;
        if !resolved.is_dir() {
            return Err(format!("Working directory '{}' is not a folder", shown(&root_path, &resolved)));
        }
        resolved
    };

    // Handle pure `cd` commands statefully while staying strictly confined inside `root_path`.
    let is_pure_cd = (command == "cd" || command.starts_with("cd "))
        && !command.contains("&&")
        && !command.contains("||")
        && !command.contains(';')
        && !command.contains('|');
    if is_pure_cd {
        let arg = command.strip_prefix("cd").unwrap_or("").trim().trim_matches('"').trim_matches('\'');
        let next_dir = if arg.is_empty() || arg == "~" || arg == "/" {
            root_path.clone()
        } else {
            let joined = work_dir.join(arg);
            let canonical = joined.canonicalize().map_err(|e| format!("cd: {arg}: {e}"))?;
            if !canonical.starts_with(&root_path) {
                return Err("cd: cannot leave the project root folder".into());
            }
            if !canonical.is_dir() {
                return Err(format!("cd: not a directory: {arg}"));
            }
            canonical
        };
        return Ok(json!({
            "ok": true,
            "command": command,
            "cwd": shown(&root_path, &next_dir),
            "stdout": "",
            "stderr": "",
            "exit_code": 0,
            "timed_out": false,
            "duration_ms": 0,
        }));
    }

    let timeout = timeout_secs.unwrap_or(DEFAULT_CMD_TIMEOUT_SECS).clamp(1, MAX_CMD_TIMEOUT_SECS);
    let started = std::time::Instant::now();

    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.args(["-lc", command]);
        c
    };
    cmd.current_dir(&work_dir)
        .env("CLICOLOR_FORCE", "1")
        .env("FORCE_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn().map_err(|e| format!("Failed to spawn command: {e}"))?;
    let wait_res = tokio::time::timeout(std::time::Duration::from_secs(timeout), child.wait_with_output()).await;
    let duration_ms = started.elapsed().as_millis() as u64;
    let cwd_shown = shown(&root_path, &work_dir);

    match wait_res {
        Err(_) => Ok(json!({
            "ok": false,
            "command": command,
            "cwd": cwd_shown,
            "stdout": "",
            "stderr": format!("Command timed out after {timeout}s"),
            "exit_code": 124,
            "timed_out": true,
            "duration_ms": duration_ms,
        })),
        Ok(Err(e)) => Err(format!("Command execution failed: {e}")),
        Ok(Ok(output)) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            if stdout.len() > MAX_CMD_OUTPUT_BYTES {
                let cut = stdout.floor_char_boundary(MAX_CMD_OUTPUT_BYTES);
                stdout.truncate(cut);
                stdout.push_str("\n… stdout truncated at 64 KB\n");
            }
            if stderr.len() > MAX_CMD_OUTPUT_BYTES {
                let cut = stderr.floor_char_boundary(MAX_CMD_OUTPUT_BYTES);
                stderr.truncate(cut);
                stderr.push_str("\n… stderr truncated at 64 KB\n");
            }
            Ok(json!({
                "ok": exit_code == 0,
                "command": command,
                "cwd": cwd_shown,
                "stdout": stdout,
                "stderr": stderr,
                "exit_code": exit_code,
                "timed_out": false,
                "duration_ms": duration_ms,
            }))
        }
    }
}

/// Runs a shell command for the `eay_project/run_command` LLM tool.
pub(crate) async fn run_shell_command(root: &Path, args: &Value) -> Result<String, String> {
    let root = root.canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    let command = args["command"].as_str().unwrap_or("").trim();
    if command.is_empty() {
        return Err("command is required".into());
    }
    let cwd = resolve(&root, args["cwd"].as_str().unwrap_or(""))?;
    if !cwd.is_dir() {
        return Err(format!("Working directory '{}' does not exist", shown(&root, &cwd)));
    }
    let timeout_secs = args["timeout_secs"].as_u64().unwrap_or(DEFAULT_CMD_TIMEOUT_SECS).clamp(1, MAX_CMD_TIMEOUT_SECS);

    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.args(["-lc", command]);
        c
    };
    cmd.current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn().map_err(|e| format!("Failed to spawn command: {e}"))?;
    let output = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child.wait_with_output())
        .await
        .map_err(|_| format!("Command timed out after {timeout_secs}s: {command}"))?
        .map_err(|e| format!("Command execution failed: {e}"))?;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut combined = String::new();
    combined.push_str(&format!("$ {command}\nexit_code: {exit_code}\n"));
    if !stdout.trim().is_empty() {
        combined.push_str("\n--- stdout ---\n");
        combined.push_str(stdout.trim_end());
        combined.push('\n');
    }
    if !stderr.trim().is_empty() {
        combined.push_str("\n--- stderr ---\n");
        combined.push_str(stderr.trim_end());
        combined.push('\n');
    }
    if combined.len() > MAX_CMD_OUTPUT_BYTES {
        let cut = combined.floor_char_boundary(MAX_CMD_OUTPUT_BYTES);
        combined.truncate(cut);
        combined.push_str("\n… output truncated at 64 KB\n");
    }
    Ok(combined)
}
