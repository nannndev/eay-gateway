//! Built-in `eay_project` agent tools (list, find, read, grep, git_status, write, edit, delete, move, run_command).

use super::{git, match_file_pattern, resolve, shown, terminal, MAX_GREP_HITS, MAX_LIST, MAX_READ_BYTES, SERVER, SKIP_DIRS};
use crate::mcp::McpToolIndex;
use serde_json::{json, Value};
use std::path::Path;

/// The tool schemas (OpenAI format) and the index that maps them back to [`SERVER`].
pub fn tools() -> (Vec<Value>, McpToolIndex) {
    let mut index = McpToolIndex::default();
    let path = json!({ "type": "string", "description": "Path relative to the project folder" });
    let defs = [
        ("list_dir", true, "List files and folders in a project directory.", json!({ "path": path })),
        (
            "find_files",
            true,
            "Recursively find files inside the project matching a filename glob or substring (e.g. '*.tsx', 'package.json'). Skips node_modules, .git, target, dist.",
            json!({
                "pattern": { "type": "string", "description": "Glob (e.g. '*.ts', '*test*') or substring to match against file path/name" },
                "path": path,
                "max_results": { "type": "integer", "description": "Maximum files to return (default 200, max 500)" }
            }),
        ),
        (
            "read_file",
            true,
            "Read a text file from the project. Use offset/limit (1-based lines) for large files.",
            json!({ "path": path, "offset": { "type": "integer" }, "limit": { "type": "integer" } }),
        ),
        (
            "grep",
            true,
            "Search project files for a plain-text pattern. Returns file:line: text.",
            json!({
                "pattern": { "type": "string" },
                "path": path,
                "case_insensitive": { "type": "boolean", "description": "Match case-insensitively when true" }
            }),
        ),
        (
            "git_status",
            true,
            "Inspect git branch, modified/untracked files, recent commits, and optional diff stat in the project folder.",
            json!({
                "include_diff": { "type": "boolean", "description": "Include git diff --stat output when true" }
            }),
        ),
        (
            "write_file",
            false,
            "Create or overwrite a file in the project with the given content.",
            json!({ "path": path, "content": { "type": "string" } }),
        ),
        (
            "edit_file",
            false,
            "Replace an exact occurrence of old_string with new_string in a project file.",
            json!({
                "path": path,
                "old_string": { "type": "string" },
                "new_string": { "type": "string" },
                "replace_all": { "type": "boolean", "description": "Replace all occurrences instead of requiring a single unique match" }
            }),
        ),
        (
            "delete_file",
            false,
            "Delete a file or empty directory inside the project folder.",
            json!({ "path": path }),
        ),
        (
            "move_file",
            false,
            "Move or rename a file or directory within the project folder.",
            json!({
                "from": { "type": "string", "description": "Source path relative to the project folder" },
                "to": { "type": "string", "description": "Destination path relative to the project folder" }
            }),
        ),
        (
            "run_command",
            false,
            "Execute a shell/terminal command inside the project folder (e.g. npm test, npm run build, cargo check, git diff). Returns exit code, stdout, and stderr.",
            json!({
                "command": { "type": "string", "description": "Shell command to run" },
                "cwd": { "type": "string", "description": "Optional relative subdirectory inside the project folder" },
                "timeout_secs": { "type": "integer", "description": "Command timeout in seconds (default 60, max 180)" }
            }),
        ),
    ];
    let tools = defs
        .into_iter()
        .map(|(name, read_only, desc, props)| {
            let required: Vec<&str> = match name {
                "list_dir" | "find_files" | "git_status" => vec![],
                "grep" => vec!["pattern"],
                "write_file" => vec!["path", "content"],
                "edit_file" => vec!["path", "old_string", "new_string"],
                "move_file" => vec!["from", "to"],
                "run_command" => vec!["command"],
                _ => vec!["path"],
            };
            json!({
                "type": "function",
                "function": {
                    "name": index.insert_with(SERVER, name, read_only),
                    "description": desc,
                    "parameters": { "type": "object", "properties": props, "required": required },
                }
            })
        })
        .collect();
    (tools, index)
}

/// Runs one tool. Errors come back as text for the model, like an MCP tool error would.
pub async fn call(root: String, tool: String, args: Value) -> String {
    match tool.as_str() {
        "run_command" => terminal::run_shell_command(Path::new(&root), &args).await.unwrap_or_else(|e| format!("Error: {e}")),
        "git_status" => git::run_git_status(Path::new(&root), &args).await.unwrap_or_else(|e| format!("Error: {e}")),
        _ => tokio::task::spawn_blocking(move || run(Path::new(&root), &tool, &args).unwrap_or_else(|e| format!("Error: {e}")))
            .await
            .unwrap_or_else(|e| format!("Error: {e}")),
    }
}

pub(crate) fn run(root: &Path, tool: &str, args: &Value) -> Result<String, String> {
    let root = root.canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    let arg = |k: &str| args[k].as_str().unwrap_or("");
    match tool {
        "list_dir" => {
            let dir = resolve(&root, arg("path"))?;
            let mut out: Vec<String> = std::fs::read_dir(&dir)
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        format!("{name}/")
                    } else {
                        name
                    }
                })
                .collect();
            out.sort();
            let total = out.len();
            out.truncate(MAX_LIST);
            if total > MAX_LIST {
                out.push(format!("… {} more", total - MAX_LIST));
            }
            Ok(if out.is_empty() { "(empty)".into() } else { out.join("\n") })
        }
        "read_file" => {
            let file = resolve(&root, arg("path"))?;
            let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", shown(&root, &file)))?;
            let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
            let limit = args["limit"].as_u64().map_or(usize::MAX, |l| l as usize);
            let mut out = String::new();
            for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
                if out.len() + line.len() > MAX_READ_BYTES {
                    out.push_str(&format!("… truncated at line {}; read more with offset", i + 1));
                    break;
                }
                out.push_str(&format!("{}\t{line}\n", i + 1));
            }
            Ok(out)
        }
        "find_files" => {
            let pattern = arg("pattern");
            let start = resolve(&root, arg("path"))?;
            let max_results = args["max_results"].as_u64().unwrap_or(200).clamp(1, MAX_LIST as u64) as usize;
            let mut found = Vec::new();
            let mut stack = vec![start];
            while let Some(p) = stack.pop() {
                if found.len() >= max_results {
                    break;
                }
                if p.is_dir() {
                    let Ok(rd) = std::fs::read_dir(&p) else { continue };
                    for e in rd.filter_map(Result::ok) {
                        let name = e.file_name();
                        let Ok(ft) = e.file_type() else { continue };
                        if ft.is_symlink() || (ft.is_dir() && SKIP_DIRS.iter().any(|s| name == *s)) {
                            continue;
                        }
                        stack.push(e.path());
                    }
                } else {
                    let rel = shown(&root, &p);
                    if match_file_pattern(pattern, &rel) {
                        found.push(rel);
                    }
                }
            }
            found.sort();
            Ok(if found.is_empty() { "No matching files".into() } else { found.join("\n") })
        }
        "grep" => {
            let pattern = arg("pattern");
            if pattern.is_empty() {
                return Err("pattern is required".into());
            }
            let case_insensitive = args["case_insensitive"].as_bool().unwrap_or(false);
            let needle_lower = if case_insensitive { pattern.to_lowercase() } else { String::new() };
            let start = resolve(&root, arg("path"))?;
            let mut hits = Vec::new();
            let mut stack = vec![start];
            while let Some(p) = stack.pop() {
                if hits.len() >= MAX_GREP_HITS {
                    break;
                }
                if p.is_dir() {
                    let Ok(rd) = std::fs::read_dir(&p) else { continue };
                    for e in rd.filter_map(Result::ok) {
                        let name = e.file_name();
                        let Ok(ft) = e.file_type() else { continue };
                        if ft.is_symlink() || (ft.is_dir() && SKIP_DIRS.iter().any(|s| name == *s)) {
                            continue;
                        }
                        stack.push(e.path());
                    }
                } else if p.metadata().is_ok_and(|m| m.len() <= 1024 * 1024) {
                    let Ok(text) = std::fs::read_to_string(&p) else { continue };
                    for (i, line) in text.lines().enumerate() {
                        let matched = if case_insensitive { line.to_lowercase().contains(&needle_lower) } else { line.contains(pattern) };
                        if matched {
                            hits.push(format!("{}:{}: {}", shown(&root, &p), i + 1, line.trim().chars().take(200).collect::<String>()));
                            if hits.len() >= MAX_GREP_HITS {
                                break;
                            }
                        }
                    }
                }
            }
            Ok(if hits.is_empty() { "No matches".into() } else { hits.join("\n") })
        }
        "write_file" => {
            let file = resolve(&root, arg("path"))?;
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let content = arg("content");
            std::fs::write(&file, content).map_err(|e| e.to_string())?;
            Ok(format!("Wrote {} ({} bytes)", shown(&root, &file), content.len()))
        }
        "edit_file" => {
            let file = resolve(&root, arg("path"))?;
            let (old, new) = (arg("old_string"), arg("new_string"));
            let replace_all = args["replace_all"].as_bool().unwrap_or(false);
            if old.is_empty() {
                return Err("old_string is required".into());
            }
            let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", shown(&root, &file)))?;
            let count = text.matches(old).count();
            match count {
                0 => Err("old_string not found".into()),
                1 => {
                    std::fs::write(&file, text.replacen(old, new, 1)).map_err(|e| e.to_string())?;
                    Ok(format!("Edited {}", shown(&root, &file)))
                }
                n if replace_all => {
                    std::fs::write(&file, text.replace(old, new)).map_err(|e| e.to_string())?;
                    Ok(format!("Edited {} ({n} replacements)", shown(&root, &file)))
                }
                n => Err(format!("old_string appears {n} times; include more context or set replace_all: true")),
            }
        }
        "delete_file" => {
            let target = resolve(&root, arg("path"))?;
            if target == root {
                return Err("Refusing to delete the project root folder".into());
            }
            if !target.exists() {
                return Err(format!("{} does not exist", shown(&root, &target)));
            }
            if target.is_dir() {
                std::fs::remove_dir(&target).map_err(|e| format!("Cannot remove dir {}: {e}", shown(&root, &target)))?;
            } else {
                std::fs::remove_file(&target).map_err(|e| format!("Cannot remove {}: {e}", shown(&root, &target)))?;
            }
            Ok(format!("Deleted {}", shown(&root, &target)))
        }
        "move_file" => {
            let from = resolve(&root, arg("from"))?;
            let to = resolve(&root, arg("to"))?;
            if from == root {
                return Err("Refusing to move the project root folder".into());
            }
            if !from.exists() {
                return Err(format!("{} does not exist", shown(&root, &from)));
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::rename(&from, &to).map_err(|e| e.to_string())?;
            Ok(format!("Moved {} -> {}", shown(&root, &from), shown(&root, &to)))
        }
        other => Err(format!("Unknown tool {other}")),
    }
}
