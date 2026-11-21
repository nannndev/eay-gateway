//! File tools for a project's folder (`root`): list, read, grep, write and edit, confined to that
//! folder. They are offered like MCP tools (`mcp__eay_project__…`), so tool policies, approvals
//! and activity work the same way.

use crate::mcp::McpToolIndex;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

/// Server name the tools are shown under.
pub const SERVER: &str = "eay_project";

const MAX_READ_BYTES: usize = 256 * 1024;
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
            "read_file",
            true,
            "Read a text file from the project. Use offset/limit (1-based lines) for large files.",
            json!({ "path": path, "offset": { "type": "integer" }, "limit": { "type": "integer" } }),
        ),
        (
            "grep",
            true,
            "Search project files for a plain-text pattern. Returns file:line: text.",
            json!({ "pattern": { "type": "string" }, "path": path }),
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
            "Replace one exact, unique occurrence of old_string with new_string in a project file.",
            json!({ "path": path, "old_string": { "type": "string" }, "new_string": { "type": "string" } }),
        ),
    ];
    let tools = defs
        .into_iter()
        .map(|(name, read_only, desc, props)| {
            let required: Vec<&str> = match name {
                "list_dir" => vec![],
                "grep" => vec!["pattern"],
                "write_file" => vec!["path", "content"],
                "edit_file" => vec!["path", "old_string", "new_string"],
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

/// Runs one tool. Errors come back as text for the model, like an MCP tool error would.
pub async fn call(root: String, tool: String, args: Value) -> String {
    tokio::task::spawn_blocking(move || run(Path::new(&root), &tool, &args).unwrap_or_else(|e| format!("Error: {e}")))
        .await
        .unwrap_or_else(|e| format!("Error: {e}"))
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
        "grep" => {
            let pattern = arg("pattern");
            if pattern.is_empty() {
                return Err("pattern is required".into());
            }
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
                        if line.contains(pattern) {
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
            if old.is_empty() {
                return Err("old_string is required".into());
            }
            let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", shown(&root, &file)))?;
            match text.matches(old).count() {
                0 => Err("old_string not found".into()),
                1 => {
                    std::fs::write(&file, text.replacen(old, new, 1)).map_err(|e| e.to_string())?;
                    Ok(format!("Edited {}", shown(&root, &file)))
                }
                n => Err(format!("old_string appears {n} times; include more context so it is unique")),
            }
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
}
