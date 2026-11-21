//! File tools for a project's folder (`root`): list, read, grep, write and edit, confined to that
//! folder. They are offered like MCP tools (`mcp__eay_project__…`), so tool policies, approvals
//! and activity work the same way.

use crate::mcp::McpToolIndex;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

/// Server name the tools are shown under.
pub const SERVER: &str = "eay_project";

const MAX_READ_BYTES: usize = 256 * 1024;
const MAX_RAW_BYTES: usize = 15 * 1024 * 1024;
const MAX_CMD_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_CMD_TIMEOUT_SECS: u64 = 60;
const MAX_CMD_TIMEOUT_SECS: u64 = 180;
const MAX_GREP_HITS: usize = 200;
const MAX_LIST: usize = 500;
/// Folders that are almost never what the model wants and can be huge.
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", "build", ".next", ".venv", "__pycache__"];

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

/// Checks a folder given for a project: absolute, and an existing directory (created first when
/// `create` is set). Returns its canonical form.
pub fn prepare_root(root: &str, create: bool) -> Result<String, String> {
    let expanded = expand_home(root.trim());
    let p = Path::new(&expanded);
    if !p.is_absolute() {
        return Err("Project root must be an absolute path".into());
    }
    if create {
        std::fs::create_dir_all(p).map_err(|e| format!("Cannot create {}: {e}", p.display()))?;
    }
    if !p.is_dir() {
        return Err(format!("Project root {} is not an existing folder", p.display()));
    }
    let canon = p.canonicalize().map_err(|e| e.to_string())?;
    Ok(canon.to_string_lossy().into_owned())
}

fn home() -> Option<String> {
    std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()
}

fn expand_home(p: &str) -> String {
    match (p == "~", p.strip_prefix("~/"), home()) {
        (true, _, Some(h)) => h,
        (_, Some(rest), Some(h)) => format!("{h}/{rest}"),
        _ => p.to_string(),
    }
}

/// Subfolders of `path` (default: the home folder), for picking a project folder in a browser,
/// which cannot hand a page an absolute path. Hidden folders are left out. Admin-only route.
pub fn list_dirs(path: Option<&str>) -> Result<Value, String> {
    let start = path.map(str::trim).filter(|p| !p.is_empty()).map(expand_home).or_else(home).ok_or("No home folder")?;
    let dir = Path::new(&start).canonicalize().map_err(|e| format!("{start}: {e}"))?;
    let mut dirs: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    dirs.sort_by_key(|n| n.to_lowercase());
    Ok(json!({
        "path": dir.to_string_lossy(),
        "parent": dir.parent().map(|p| p.to_string_lossy().into_owned()),
        "home": home(),
        "dirs": dirs,
    }))
}

/// What the UI shows for `rel` inside a project's `root`: a directory's entries (folders first,
/// heavy build folders left out), or a file's text (cut at 256 KB). Same confinement as the tools.
pub fn browse(root: &str, rel: &str) -> Result<Value, String> {
    let root = Path::new(root).canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    let target = resolve(&root, rel)?;
    if target.is_dir() {
        let mut entries: Vec<(bool, String)> = std::fs::read_dir(&target)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .map(|e| (e.path().is_dir(), e.file_name().to_string_lossy().into_owned()))
            .filter(|(dir, n)| !(*dir && SKIP_DIRS.contains(&n.as_str())))
            .collect();
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        entries.truncate(MAX_LIST);
        let entries: Vec<Value> = entries.into_iter().map(|(dir, name)| json!({ "name": name, "dir": dir })).collect();
        return Ok(json!({ "path": shown(&root, &target), "entries": entries }));
    }
    let bytes = std::fs::read(&target).map_err(|e| format!("{}: {e}", shown(&root, &target)))?;
    let truncated = bytes.len() > MAX_READ_BYTES;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_READ_BYTES)]).into_owned();
    Ok(json!({ "path": shown(&root, &target), "content": text, "size": bytes.len(), "truncated": truncated }))
}

/// Writes a text file confined strictly inside `root` (for `PUT /gateway/projects/:id/files`).
pub fn write_text_file(root: &str, rel: &str, content: &str) -> Result<Value, String> {
    let root = Path::new(root).canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    let target = resolve(&root, rel)?;
    if target == root {
        return Err("Refusing to overwrite the project root folder".into());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&target, content).map_err(|e| format!("{}: {e}", shown(&root, &target)))?;
    Ok(json!({ "path": shown(&root, &target), "bytes": content.len(), "saved": true }))
}

fn mime_for_path(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "json" => "application/json",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Reads raw bytes for binary preview (`GET /gateway/projects/:id/files/raw?path=`), capped at 15 MB.
pub fn read_raw_file(root: &str, rel: &str) -> Result<(Vec<u8>, &'static str), String> {
    let root = Path::new(root).canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    let target = resolve(&root, rel)?;
    if !target.is_file() {
        return Err(format!("{} is not a file", shown(&root, &target)));
    }
    let meta = std::fs::metadata(&target).map_err(|e| e.to_string())?;
    if meta.len() as usize > MAX_RAW_BYTES {
        return Err("File exceeds 15 MB raw preview limit".into());
    }
    let bytes = std::fs::read(&target).map_err(|e| format!("{}: {e}", shown(&root, &target)))?;
    Ok((bytes, mime_for_path(&target)))
}

fn run_git_sync(root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("git failed: {e}"))
}

fn parse_numstat(stdout: &str, map: &mut std::collections::HashMap<String, (u64, u64)>) {
    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(a), Some(r), Some(p)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let add = a.parse::<u64>().unwrap_or(0);
        let rem = r.parse::<u64>().unwrap_or(0);
        let entry = map.entry(p.trim().replace('\\', "/")).or_insert((0, 0));
        entry.0 += add;
        entry.1 += rem;
    }
}

/// Returns real working-tree git status, branch info, and per-file `+added/-removed` counts.
pub fn git_changes(root: &str) -> Result<Value, String> {
    let root = Path::new(root).canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    let check = run_git_sync(&root, &["rev-parse", "--is-inside-work-tree"]);
    let is_git = check.is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true");
    if !is_git {
        return Ok(json!({ "is_git": false, "branch": null, "ahead": 0, "behind": 0, "changes": [] }));
    }

    let status_out = run_git_sync(&root, &["status", "--porcelain=v1", "-b", "-uall"])?;
    let status_text = String::from_utf8_lossy(&status_out.stdout);

    let mut branch: Option<String> = None;
    let mut ahead: u64 = 0;
    let mut behind: u64 = 0;
    let mut numstat = std::collections::HashMap::<String, (u64, u64)>::new();
    if let Ok(o) = run_git_sync(&root, &["diff", "--numstat"]) {
        parse_numstat(&String::from_utf8_lossy(&o.stdout), &mut numstat);
    }
    if let Ok(o) = run_git_sync(&root, &["diff", "--cached", "--numstat"]) {
        parse_numstat(&String::from_utf8_lossy(&o.stdout), &mut numstat);
    }

    let mut changes = Vec::new();
    for line in status_text.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            let main_part = header.split("...").next().unwrap_or(header);
            let b = main_part.split_whitespace().next().unwrap_or(main_part).trim();
            if !b.is_empty() && b != "HEAD" {
                branch = Some(b.to_string());
            } else if header.contains("No commits yet on ") {
                branch = header.split("No commits yet on ").nth(1).map(|s| s.trim().to_string());
            }
            if let Some(bracket) = header.split('[').nth(1).and_then(|s| s.split(']').next()) {
                for seg in bracket.split(',') {
                    let s = seg.trim();
                    if let Some(n) = s.strip_prefix("ahead ") {
                        ahead = n.trim().parse().unwrap_or(0);
                    } else if let Some(n) = s.strip_prefix("behind ") {
                        behind = n.trim().parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        let bytes = line.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        let raw_path = line[3..].trim().trim_matches('"');
        let path = raw_path.rsplit(" -> ").next().unwrap_or(raw_path).replace('\\', "/");
        if path.is_empty() {
            continue;
        }
        let staged = x != ' ' && x != '?';
        let status = match (x, y) {
            ('?', '?') => "U",
            ('A', _) => "A",
            ('D', _) | (_, 'D') => "D",
            ('R', _) => "R",
            _ => "M",
        };
        let (added, removed) = numstat.get(&path).copied().unwrap_or((0, 0));
        changes.push(json!({
            "path": path,
            "status": status,
            "staged": staged,
            "added": added,
            "removed": removed,
        }));
    }

    Ok(json!({
        "is_git": true,
        "branch": branch,
        "ahead": ahead,
        "behind": behind,
        "changes": changes,
    }))
}

/// Returns unified diff plus `HEAD` original vs current working-tree text for a single file.
pub fn git_file_diff(root: &str, rel: &str) -> Result<Value, String> {
    let root = Path::new(root).canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    let target = resolve(&root, rel)?;
    let rel_norm = shown(&root, &target);

    let head_spec = format!("HEAD:{rel_norm}");
    let original = run_git_sync(&root, &["show", &head_spec])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    let modified = if target.is_file() {
        let bytes = std::fs::read(&target).unwrap_or_default();
        String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_READ_BYTES)]).into_owned()
    } else {
        String::new()
    };

    let diff = run_git_sync(&root, &["diff", "HEAD", "--", &rel_norm])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    Ok(json!({
        "path": rel_norm,
        "diff": diff,
        "original": original,
        "modified": modified,
    }))
}

/// Runs a structured, safe Git action inside `root` (stage, unstage, discard, commit, push, pull,
/// branches, checkout, create_branch, log, show_commit, stash, stash_pop, stash_list, diff_summary).
#[allow(clippy::too_many_arguments)]
pub fn git_action(
    root: &str,
    action: &str,
    paths: &[String],
    message: Option<&str>,
    stage_all: bool,
    branch: Option<&str>,
    commit: Option<&str>,
    limit: Option<u32>,
) -> Result<Value, String> {
    let root = Path::new(root).canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    let mut clean_paths: Vec<String> = Vec::new();
    for p in paths {
        let resolved = resolve(&root, p)?;
        let rel = shown(&root, &resolved);
        if !rel.is_empty() {
            clean_paths.push(rel);
        }
    }

    let exec = |args: &[String]| -> Result<String, String> {
        let str_args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run_git_sync(&root, &str_args)?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if !out.status.success() {
            return Err(if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                format!("git {} failed", args.join(" "))
            });
        }
        Ok(if !stdout.is_empty() { stdout } else { stderr })
    };

    let validate_ref = |raw: Option<&str>, label: &str| -> Result<String, String> {
        let val = raw.map(str::trim).unwrap_or("");
        if val.is_empty() {
            return Err(format!("{label} is required"));
        }
        if val.starts_with('-') || val.contains("..") || val.contains(char::is_whitespace) {
            return Err(format!("Invalid {label}: '{val}'"));
        }
        Ok(val.to_string())
    };

    match action {
        "branches" => {
            let out = exec(&[
                "for-each-ref".into(),
                "--sort=-committerdate".into(),
                "--format=%(HEAD)\t%(refname:short)\t%(upstream:short)".into(),
                "refs/heads".into(),
            ])?;
            let mut branches = Vec::new();
            let mut current: Option<String> = None;
            for line in out.lines() {
                let mut parts = line.split('\t');
                let head = parts.next().unwrap_or("").trim();
                let name = parts.next().unwrap_or("").trim();
                let upstream = parts.next().unwrap_or("").trim();
                if name.is_empty() {
                    continue;
                }
                let is_cur = head == "*";
                if is_cur {
                    current = Some(name.to_string());
                }
                branches.push(json!({
                    "name": name,
                    "current": is_cur,
                    "upstream": if upstream.is_empty() { Value::Null } else { Value::String(upstream.to_string()) },
                }));
            }
            return Ok(json!({ "ok": true, "action": action, "current": current, "branches": branches, "output": "" }));
        }
        "checkout" => {
            let b = validate_ref(branch, "Branch name")?;
            let output = exec(&["checkout".into(), b])?;
            return Ok(json!({ "ok": true, "action": action, "output": output }));
        }
        "create_branch" => {
            let b = validate_ref(branch, "Branch name")?;
            let output = exec(&["checkout".into(), "-b".into(), b])?;
            return Ok(json!({ "ok": true, "action": action, "output": output }));
        }
        "log" => {
            let n = limit.unwrap_or(20).clamp(1, 100);
            let out = run_git_sync(&root, &["log", "-n", &n.to_string(), "--pretty=format:%H\x1f%h\x1f%s\x1f%an\x1f%cr"])?;
            let mut commits = Vec::new();
            if out.status.success() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    let parts: Vec<&str> = line.split('\x1f').collect();
                    if parts.len() >= 5 {
                        commits.push(json!({
                            "hash": parts[0],
                            "short_hash": parts[1],
                            "subject": parts[2],
                            "author": parts[3],
                            "date_relative": parts[4],
                        }));
                    }
                }
            }
            return Ok(json!({ "ok": true, "action": action, "commits": commits, "output": "" }));
        }
        "show_commit" => {
            let h = validate_ref(commit, "Commit hash")?;
            let mut output = exec(&["show".into(), "--stat".into(), "--patch".into(), h])?;
            if output.len() > MAX_CMD_OUTPUT_BYTES {
                let cut = output.floor_char_boundary(MAX_CMD_OUTPUT_BYTES);
                output.truncate(cut);
                output.push_str("\n… commit diff truncated at 64 KB\n");
            }
            return Ok(json!({ "ok": true, "action": action, "output": output }));
        }
        "stash" => {
            let msg = message.map(str::trim).filter(|m| !m.is_empty()).unwrap_or("eay-workspace stash");
            let output = exec(&["stash".into(), "push".into(), "-u".into(), "-m".into(), msg.into()])?;
            return Ok(json!({ "ok": true, "action": action, "output": output }));
        }
        "stash_pop" => {
            let output = exec(&["stash".into(), "pop".into()])?;
            return Ok(json!({ "ok": true, "action": action, "output": output }));
        }
        "stash_list" => {
            let out = run_git_sync(&root, &["stash", "list", "--pretty=format:%gd\x1f%gs"])?;
            let mut stashes = Vec::new();
            if out.status.success() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    let mut parts = line.splitn(2, '\x1f');
                    if let (Some(r), Some(m)) = (parts.next(), parts.next()) {
                        stashes.push(json!({ "ref": r, "message": m }));
                    }
                }
            }
            return Ok(json!({ "ok": true, "action": action, "stashes": stashes, "output": "" }));
        }
        "diff_summary" => {
            let status = run_git_sync(&root, &["status", "--short"])
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            let staged_diff = run_git_sync(&root, &["diff", "--cached"])
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            let raw_diff = if !staged_diff.trim().is_empty() {
                staged_diff
            } else {
                run_git_sync(&root, &["diff"]).ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
            };
            let mut combined = format!("Status:\n{status}\nDiff:\n{raw_diff}");
            if combined.len() > 12_000 {
                let cut = combined.floor_char_boundary(12_000);
                combined.truncate(cut);
                combined.push_str("\n… truncated");
            }
            return Ok(json!({ "ok": true, "action": action, "output": combined }));
        }
        _ => {}
    }

    let validate_ref = |raw: Option<&str>, label: &str| -> Result<String, String> {
        let val = raw.map(str::trim).unwrap_or("");
        if val.is_empty() {
            return Err(format!("{label} is required"));
        }
        if val.starts_with('-') || val.contains("..") || val.contains(char::is_whitespace) {
            return Err(format!("Invalid {label}: '{val}'"));
        }
        Ok(val.to_string())
    };

    match action {
        "branches" => {
            let out = exec(&[
                "for-each-ref".into(),
                "--sort=-committerdate".into(),
                "--format=%(HEAD)\t%(refname:short)\t%(upstream:short)".into(),
                "refs/heads".into(),
            ])?;
            let mut branches = Vec::new();
            let mut current: Option<String> = None;
            for line in out.lines() {
                let mut parts = line.split('\t');
                let head = parts.next().unwrap_or("").trim();
                let name = parts.next().unwrap_or("").trim();
                let upstream = parts.next().unwrap_or("").trim();
                if name.is_empty() {
                    continue;
                }
                let is_cur = head == "*";
                if is_cur {
                    current = Some(name.to_string());
                }
                branches.push(json!({
                    "name": name,
                    "current": is_cur,
                    "upstream": if upstream.is_empty() { Value::Null } else { Value::String(upstream.to_string()) },
                }));
            }
            return Ok(json!({ "ok": true, "action": action, "current": current, "branches": branches, "output": "" }));
        }
        "checkout" => {
            let b = validate_ref(branch, "Branch name")?;
            let output = exec(&["checkout".into(), b])?;
            return Ok(json!({ "ok": true, "action": action, "output": output }));
        }
        "create_branch" => {
            let b = validate_ref(branch, "Branch name")?;
            let output = exec(&["checkout".into(), "-b".into(), b])?;
            return Ok(json!({ "ok": true, "action": action, "output": output }));
        }
        "log" => {
            let n = limit.unwrap_or(20).clamp(1, 100);
            let out = run_git_sync(
                &root,
                &["log", "-n", &n.to_string(), "--pretty=format:%H\x1f%h\x1f%s\x1f%an\x1f%cr"],
            )?;
            let mut commits = Vec::new();
            if out.status.success() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    let parts: Vec<&str> = line.split('\x1f').collect();
                    if parts.len() >= 5 {
                        commits.push(json!({
                            "hash": parts[0],
                            "short_hash": parts[1],
                            "subject": parts[2],
                            "author": parts[3],
                            "date_relative": parts[4],
                        }));
                    }
                }
            }
            return Ok(json!({ "ok": true, "action": action, "commits": commits, "output": "" }));
        }
        "show_commit" => {
            let h = validate_ref(commit, "Commit hash")?;
            let mut output = exec(&["show".into(), "--stat".into(), "--patch".into(), h])?;
            if output.len() > MAX_CMD_OUTPUT_BYTES {
                let cut = output.floor_char_boundary(MAX_CMD_OUTPUT_BYTES);
                output.truncate(cut);
                output.push_str("\n… commit diff truncated at 64 KB\n");
            }
            return Ok(json!({ "ok": true, "action": action, "output": output }));
        }
        "stash" => {
            let msg = message.map(str::trim).filter(|m| !m.is_empty()).unwrap_or("eay-workspace stash");
            let output = exec(&["stash".into(), "push".into(), "-u".into(), "-m".into(), msg.into()])?;
            return Ok(json!({ "ok": true, "action": action, "output": output }));
        }
        "stash_pop" => {
            let output = exec(&["stash".into(), "pop".into()])?;
            return Ok(json!({ "ok": true, "action": action, "output": output }));
        }
        "stash_list" => {
            let out = run_git_sync(&root, &["stash", "list", "--pretty=format:%gd\x1f%gs"])?;
            let mut stashes = Vec::new();
            if out.status.success() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    let mut parts = line.splitn(2, '\x1f');
                    if let (Some(r), Some(m)) = (parts.next(), parts.next()) {
                        stashes.push(json!({ "ref": r, "message": m }));
                    }
                }
            }
            return Ok(json!({ "ok": true, "action": action, "stashes": stashes, "output": "" }));
        }
        "diff_summary" => {
            let status = run_git_sync(&root, &["status", "--short"])
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            let staged_diff = run_git_sync(&root, &["diff", "--cached"])
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            let raw_diff = if !staged_diff.trim().is_empty() {
                staged_diff
            } else {
                run_git_sync(&root, &["diff"])
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                    .unwrap_or_default()
            };
            let mut combined = format!("Status:\n{status}\nDiff:\n{raw_diff}");
            if combined.len() > 12_000 {
                let cut = combined.floor_char_boundary(12_000);
                combined.truncate(cut);
                combined.push_str("\n… truncated");
            }
            return Ok(json!({ "ok": true, "action": action, "output": combined }));
        }
        _ => {}
    }

    let output = match action {
        "stage" => {
            if clean_paths.is_empty() {
                exec(&["add".into(), "-A".into()])?
            } else {
                let mut args = vec!["add".into(), "--".into()];
                args.extend(clean_paths);
                exec(&args)?
            }
        }
        "unstage" => {
            if clean_paths.is_empty() {
                exec(&["reset".into(), "HEAD".into()])?
            } else {
                let mut args = vec!["reset".into(), "HEAD".into(), "--".into()];
                args.extend(clean_paths);
                exec(&args)?
            }
        }
        "discard" => {
            if clean_paths.is_empty() {
                return Err("Specify at least one path to discard".into());
            }
            for rel in &clean_paths {
                let tracked = run_git_sync(&root, &["ls-files", "--error-unmatch", "--", rel]).is_ok_and(|o| o.status.success());
                if tracked {
                    let _ = exec(&["reset".into(), "HEAD".into(), "--".into(), rel.clone()]);
                    exec(&["checkout".into(), "--".into(), rel.clone()])?;
                } else {
                    let target = resolve(&root, rel)?;
                    if target != root && target.is_file() {
                        std::fs::remove_file(&target).map_err(|e| e.to_string())?;
                    }
                }
            }
            format!("Discarded {} file(s)", clean_paths.len())
        }
        "commit" => {
            let msg = message.map(str::trim).unwrap_or("");
            if msg.is_empty() {
                return Err("Commit message is required".into());
            }
            if stage_all {
                exec(&["add".into(), "-A".into()])?;
            }
            exec(&["commit".into(), "-m".into(), msg.into()])?
        }
        "push" => exec(&["push".into()])?,
        "pull" => exec(&["pull".into(), "--ff-only".into()])?,
        other => return Err(format!("Unknown git action '{other}'")),
    };

    Ok(json!({ "ok": true, "action": action, "output": output }))
}

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

/// Resolves `rel` inside `root`, refusing anything that ends up outside it (`..`, absolute paths
/// elsewhere, symlinks pointing out). The target itself may not exist yet (for writes).
fn resolve(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim();
    let candidate = if rel.is_empty() || rel == "." { root.to_path_buf() } else { root.join(rel) };
    if candidate.components().any(|c| c == Component::ParentDir) {
        return Err("Paths with `..` are not allowed".into());
    }
    // Canonicalize the deepest part that exists, so a symlink cannot lead outside the root.
    let mut existing = candidate.as_path();
    let mut rest = Vec::new();
    while !existing.exists() {
        rest.push(existing.file_name().ok_or("Invalid path")?.to_owned());
        existing = existing.parent().ok_or("Invalid path")?;
    }
    let mut real = existing.canonicalize().map_err(|e| e.to_string())?;
    if !real.starts_with(root) {
        return Err(format!("{rel} is outside the project folder"));
    }
    real.extend(rest.into_iter().rev());
    Ok(real)
}

/// Path relative to the project, with `/` on every OS so the model and the UI see one form.
fn shown(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

/// Simple `*` glob or substring matcher for `find_files`.
fn match_file_pattern(pattern: &str, rel_path: &str) -> bool {
    let pat = pattern.trim().to_lowercase();
    if pat.is_empty() || pat == "*" {
        return true;
    }
    let rel_lower = rel_path.to_lowercase();
    let name_lower = rel_lower.rsplit('/').next().unwrap_or(&rel_lower);
    if !pat.contains('*') {
        return rel_lower.contains(&pat);
    }
    let matches_glob = |target: &str| {
        let parts: Vec<&str> = pat.split('*').collect();
        let mut pos = 0usize;
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            if i == 0 {
                if !target.starts_with(part) {
                    return false;
                }
                pos = part.len();
            } else if i == parts.len() - 1 {
                if !target[pos..].ends_with(part) {
                    return false;
                }
                pos = target.len();
            } else if let Some(idx) = target[pos..].find(part) {
                pos += idx + part.len();
            } else {
                return false;
            }
        }
        true
    };
    matches_glob(name_lower) || matches_glob(&rel_lower)
}

async fn run_shell_command(root: &Path, args: &Value) -> Result<String, String> {
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

async fn run_git_status(root: &Path, args: &Value) -> Result<String, String> {
    let root = root.canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    let include_diff = args["include_diff"].as_bool().unwrap_or(false);

    let run_git = |git_args: &[&str]| {
        let dir = root.clone();
        let owned: Vec<String> = git_args.iter().map(|s| s.to_string()).collect();
        async move {
            let out = tokio::process::Command::new("git")
                .args(&owned)
                .current_dir(&dir)
                .stdin(std::process::Stdio::null())
                .output()
                .await
                .map_err(|e| format!("git failed: {e}"))?;
            let text = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
            let err = String::from_utf8_lossy(&out.stderr).trim_end().to_string();
            if !out.status.success() {
                return Err(if err.is_empty() { text } else { err });
            }
            Ok(text)
        }
    };

    let status = run_git(&["status", "--short", "--branch"]).await?;
    let log = run_git(&["log", "-n", "5", "--oneline"]).await.unwrap_or_default();
    let mut out = format!("=== git status ===\n{status}\n");
    if !log.is_empty() {
        out.push_str(&format!("\n=== recent commits ===\n{log}\n"));
    }
    if include_diff {
        if let Ok(diff) = run_git(&["diff", "--stat"]).await {
            if !diff.is_empty() {
                out.push_str(&format!("\n=== diff stat ===\n{diff}\n"));
            }
        }
    }
    Ok(out)
}

/// Runs one tool. Errors come back as text for the model, like an MCP tool error would.
pub async fn call(root: String, tool: String, args: Value) -> String {
    match tool.as_str() {
        "run_command" => run_shell_command(Path::new(&root), &args).await.unwrap_or_else(|e| format!("Error: {e}")),
        "git_status" => run_git_status(Path::new(&root), &args).await.unwrap_or_else(|e| format!("Error: {e}")),
        _ => tokio::task::spawn_blocking(move || run(Path::new(&root), &tool, &args).unwrap_or_else(|e| format!("Error: {e}")))
            .await
            .unwrap_or_else(|e| format!("Error: {e}")),
    }
}

fn run(root: &Path, tool: &str, args: &Value) -> Result<String, String> {
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
                        // Symlinks are skipped so the walk never leaves the folder.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_stay_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let r = |t: &str, a: Value| run(&root, t, &a);

        assert!(r("write_file", json!({ "path": "src/a.txt", "content": "hello\nworld" })).is_ok());
        assert_eq!(r("read_file", json!({ "path": "src/a.txt" })).unwrap(), "1\thello\n2\tworld\n");
        assert!(r("list_dir", json!({})).unwrap().contains("src/"));
        assert_eq!(r("grep", json!({ "pattern": "world" })).unwrap(), "src/a.txt:2: world");
        assert!(r("edit_file", json!({ "path": "src/a.txt", "old_string": "world", "new_string": "there" })).is_ok());
        assert!(r("read_file", json!({ "path": "src/a.txt" })).unwrap().contains("there"));
        assert!(r("edit_file", json!({ "path": "src/a.txt", "old_string": "nope", "new_string": "x" })).is_err());
        assert_eq!(r("find_files", json!({ "pattern": "*.txt" })).unwrap(), "src/a.txt");
        assert!(r("move_file", json!({ "from": "src/a.txt", "to": "src/b.txt" })).is_ok());
        assert_eq!(r("find_files", json!({ "pattern": "*.txt" })).unwrap(), "src/b.txt");
        assert!(r("delete_file", json!({ "path": "src/b.txt" })).is_ok());
        assert_eq!(r("find_files", json!({ "pattern": "*.txt" })).unwrap(), "No matching files");
        assert!(r("delete_file", json!({ "path": "" })).is_err(), "cannot delete project root");

        assert!(r("read_file", json!({ "path": "../x" })).is_err(), "`..` is refused");
        assert!(r("read_file", json!({ "path": "/etc/hosts" })).is_err(), "absolute paths outside root are refused");
        assert!(r("write_file", json!({ "path": "/tmp/eay-escape.txt", "content": "x" })).is_err());

        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("secret"), "s").unwrap();
            std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
            assert!(r("read_file", json!({ "path": "link/secret" })).is_err(), "symlinks out of root are refused");
            assert!(r("write_file", json!({ "path": "link/new", "content": "x" })).is_err());
        }
    }

    #[test]
    fn browse_lists_folders_first_and_reads_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        std::fs::write(dir.path().join("a.md"), "hi").unwrap();
        let listed = browse(root, "").unwrap();
        assert_eq!(listed["entries"], json!([{ "name": "src", "dir": true }, { "name": "a.md", "dir": false }]));
        assert_eq!(browse(root, "a.md").unwrap()["content"], "hi");
        assert!(browse(root, "../").is_err());
    }

    #[test]
    fn prepare_root_checks_and_creates() {
        let dir = tempfile::tempdir().unwrap();
        assert!(prepare_root("relative/path", false).is_err());
        let missing = dir.path().join("new");
        assert!(prepare_root(missing.to_str().unwrap(), false).is_err());
        assert!(prepare_root(missing.to_str().unwrap(), true).is_ok());
        assert!(missing.is_dir());
        std::fs::create_dir(missing.join(".hidden")).unwrap();
        std::fs::create_dir(missing.join("app")).unwrap();
        let listed = list_dirs(missing.to_str()).unwrap();
        assert_eq!(listed["dirs"], json!(["app"]), "hidden folders are left out");
        assert!(list_dirs(Some("/definitely/not/here")).is_err());
    }

    #[tokio::test]
    async fn run_command_executes_inside_root_and_refuses_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let out = call(root.to_string_lossy().into_owned(), "run_command".into(), json!({ "command": "echo hello-eay" })).await;
        assert!(out.contains("exit_code: 0") && out.contains("hello-eay"), "{out}");
        let escaped = call(root.to_string_lossy().into_owned(), "run_command".into(), json!({ "command": "pwd", "cwd": "../.." })).await;
        assert!(escaped.starts_with("Error:"), "{escaped}");
    }
}
