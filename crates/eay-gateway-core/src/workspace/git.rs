//! Real Git inspection and safe structured Git actions confined to a project's root folder.

use super::{resolve, shown, MAX_CMD_OUTPUT_BYTES, MAX_READ_BYTES};
use serde_json::{json, Value};
use std::path::Path;

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

/// Async git status helper used by the `eay_project/git_status` LLM tool.
pub(crate) async fn run_git_status(root: &Path, args: &Value) -> Result<String, String> {
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
