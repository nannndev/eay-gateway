//! Path-confined filesystem operations for project browsing, editing, CRUD, binary preview,
//! and project-wide text search.

use super::{resolve, shown, MAX_GREP_HITS, MAX_LIST, MAX_RAW_BYTES, MAX_READ_BYTES, SKIP_DIRS};
use serde_json::{json, Value};
use std::path::Path;

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

/// Creates a directory (and missing parents) strictly inside `root`.
pub fn create_dir(root: &str, rel: &str) -> Result<Value, String> {
    let root = Path::new(root).canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    if rel.trim().is_empty() {
        return Err("path is required".into());
    }
    let target = resolve(&root, rel)?;
    if target == root {
        return Err("Project root already exists".into());
    }
    std::fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", shown(&root, &target)))?;
    Ok(json!({ "path": shown(&root, &target), "created": true }))
}

/// Deletes a file or directory strictly inside `root` (refuses deleting root or `.git`).
pub fn delete_entry(root: &str, rel: &str, recursive: bool) -> Result<Value, String> {
    let root = Path::new(root).canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    if rel.trim().is_empty() {
        return Err("Refusing to delete the project root folder".into());
    }
    let target = resolve(&root, rel)?;
    if target == root {
        return Err("Refusing to delete the project root folder".into());
    }
    let rel_shown = shown(&root, &target);
    if rel_shown == ".git" || rel_shown.starts_with(".git/") {
        return Err("Refusing to delete .git metadata".into());
    }
    if !target.exists() {
        return Err(format!("{rel_shown} does not exist"));
    }
    if target.is_dir() {
        if recursive {
            std::fs::remove_dir_all(&target).map_err(|e| format!("Cannot remove {rel_shown}: {e}"))?;
        } else {
            std::fs::remove_dir(&target).map_err(|e| format!("Cannot remove {rel_shown}: {e}"))?;
        }
    } else {
        std::fs::remove_file(&target).map_err(|e| format!("Cannot remove {rel_shown}: {e}"))?;
    }
    Ok(json!({ "path": rel_shown, "deleted": true }))
}

/// Renames or moves a file or directory strictly inside `root`.
pub fn rename_entry(root: &str, from_rel: &str, to_rel: &str) -> Result<Value, String> {
    let root = Path::new(root).canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    let from = resolve(&root, from_rel)?;
    let to = resolve(&root, to_rel)?;
    if from == root || to == root {
        return Err("Refusing to move the project root folder".into());
    }
    if !from.exists() {
        return Err(format!("{} does not exist", shown(&root, &from)));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&from, &to).map_err(|e| e.to_string())?;
    Ok(json!({ "from": shown(&root, &from), "to": shown(&root, &to), "renamed": true }))
}

/// Structured project-wide text search (Global Find-in-Files) skipping heavy build folders.
pub fn search_text(
    root: &str,
    query: &str,
    rel_dir: Option<&str>,
    case_insensitive: bool,
    max_results: Option<usize>,
) -> Result<Value, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("q is required".into());
    }
    let root = Path::new(root).canonicalize().map_err(|e| format!("Project folder unavailable: {e}"))?;
    let start = resolve(&root, rel_dir.unwrap_or(""))?;
    let limit = max_results.unwrap_or(MAX_GREP_HITS).clamp(1, 500);
    let needle_lower = if case_insensitive { q.to_lowercase() } else { String::new() };

    let mut matches = Vec::new();
    let mut truncated = false;
    let mut stack = vec![start];
    while let Some(p) = stack.pop() {
        if matches.len() >= limit {
            truncated = true;
            break;
        }
        if p.is_dir() {
            let Ok(rd) = std::fs::read_dir(&p) else { continue };
            let mut children: Vec<_> = rd.filter_map(Result::ok).collect();
            children.sort_by_key(|e| e.file_name());
            for e in children.into_iter().rev() {
                let name = e.file_name();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_symlink() || (ft.is_dir() && SKIP_DIRS.iter().any(|s| name == *s)) {
                    continue;
                }
                stack.push(e.path());
            }
        } else if p.metadata().is_ok_and(|m| m.len() <= 1024 * 1024) {
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let rel = shown(&root, &p);
            for (i, line) in text.lines().enumerate() {
                let col = if case_insensitive { line.to_lowercase().find(&needle_lower) } else { line.find(q) };
                if let Some(byte_col) = col {
                    matches.push(json!({
                        "path": rel,
                        "line": i + 1,
                        "column": byte_col + 1,
                        "text": line.trim().chars().take(220).collect::<String>(),
                    }));
                    if matches.len() >= limit {
                        truncated = true;
                        break;
                    }
                }
            }
        }
    }

    Ok(json!({
        "query": q,
        "matches": matches,
        "truncated": truncated,
    }))
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
