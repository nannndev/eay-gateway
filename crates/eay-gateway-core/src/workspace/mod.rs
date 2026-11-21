//! Project workspace engine: path-confined filesystem browsing/editing/search,
//! real Git operations, interactive terminal execution, and built-in `eay_project` agent tools.

mod fs;
mod git;
mod terminal;
mod tools;

pub use fs::{browse, create_dir, delete_entry, list_dirs, prepare_root, read_raw_file, rename_entry, search_text, write_text_file};
pub use git::{git_action, git_changes, git_file_diff};
pub use terminal::exec_terminal;
pub use tools::{call, tools};

use std::path::{Component, Path, PathBuf};

/// Server name the built-in project tools are shown under.
pub const SERVER: &str = "eay_project";

pub(crate) const MAX_READ_BYTES: usize = 256 * 1024;
pub(crate) const MAX_RAW_BYTES: usize = 15 * 1024 * 1024;
pub(crate) const MAX_CMD_OUTPUT_BYTES: usize = 64 * 1024;
pub(crate) const DEFAULT_CMD_TIMEOUT_SECS: u64 = 60;
pub(crate) const MAX_CMD_TIMEOUT_SECS: u64 = 180;
pub(crate) const MAX_GREP_HITS: usize = 200;
pub(crate) const MAX_LIST: usize = 500;
/// Folders that are almost never what the model or UI search wants and can be huge.
pub(crate) const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", "build", ".next", ".venv", "__pycache__"];

/// Resolves `rel` inside `root`, refusing anything that ends up outside it (`..`, absolute paths
/// elsewhere, symlinks pointing out). The target itself may not exist yet (for writes).
pub(crate) fn resolve(root: &Path, rel: &str) -> Result<PathBuf, String> {
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
pub(crate) fn shown(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

/// Simple `*` glob or substring matcher for `find_files`.
pub(crate) fn match_file_pattern(pattern: &str, rel_path: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn tools_stay_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let r = |t: &str, a: Value| tools::run(&root, t, &a);

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
    fn fs_crud_and_search_work_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        assert!(create_dir(root, "src/utils").is_ok());
        assert!(write_text_file(root, "src/utils/math.ts", "export const add = (a: number, b: number) => a + b;\n").is_ok());
        let hits = search_text(root, "export const add", None, false, Some(50)).unwrap();
        assert_eq!(hits["matches"].as_array().unwrap().len(), 1);
        assert_eq!(hits["matches"][0]["path"], "src/utils/math.ts");
        assert_eq!(hits["matches"][0]["line"], 1);
        assert!(rename_entry(root, "src/utils/math.ts", "src/utils/calc.ts").is_ok());
        assert!(delete_entry(root, "src/utils/calc.ts", false).is_ok());
        assert!(delete_entry(root, "", true).is_err());
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
