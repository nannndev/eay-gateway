//! `eay-gateway service install|uninstall|status`: start the gateway at login, for the current user
//! only (no admin rights). macOS uses a LaunchAgent, Linux a systemd user unit, Windows a scheduled
//! task that runs at logon.

use std::path::{Path, PathBuf};
use std::process::Command;

const LABEL: &str = "dev.nannndev.eay-gateway";
// Built on every OS so each platform's file format is unit-tested everywhere; only one is used per build.
#[allow(dead_code)]
const UNIT: &str = "eay-gateway.service";
// Built on every OS so each platform's file format is unit-tested everywhere; only one is used per build.
#[allow(dead_code)]
const TASK: &str = "eay-gateway";

/// Runs `service <action> [gateway args…]` and returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let (action, rest) = match args.split_first() {
        Some((a, rest)) => (a.as_str(), rest),
        None => ("help", &[][..]),
    };
    let result = match action {
        "install" => install(rest),
        "uninstall" => uninstall(),
        "status" => status(),
        _ => {
            println!(
                "USAGE:\n  eay-gateway service install [OPTIONS]   Start at login with these gateway options\n  \
                 eay-gateway service uninstall           Stop and remove the login item\n  \
                 eay-gateway service status              Show whether it is installed and running\n\n\
                 Options after `install` are passed to the gateway, e.g.\n  \
                 eay-gateway service install --port 20129 --console-origin https://nannndev.github.io"
            );
            return if action == "help" || action == "-h" || action == "--help" { 0 } else { 2 };
        }
    };
    match result {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))] // Windows uses Task Scheduler, no files
fn home() -> Result<PathBuf, String> {
    std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).map(PathBuf::from).map_err(|_| "cannot find your home folder".into())
}

/// The binary to register. A Homebrew install runs from a versioned Cellar path that disappears on
/// upgrade, so prefer the stable `opt` symlink when there is one.
fn current_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program's path: {e}"))?;
    Ok(stable_brew_path(&exe).unwrap_or(exe))
}

/// `/opt/homebrew/Cellar/eay-gateway/0.3.0/bin/eay-gateway` → `/opt/homebrew/opt/eay-gateway/bin/eay-gateway`.
fn stable_brew_path(exe: &Path) -> Option<PathBuf> {
    let s = exe.to_str()?;
    let (prefix, rest) = s.split_once("/Cellar/")?;
    let (formula, after) = rest.split_once('/')?;
    let (_version, tail) = after.split_once('/')?;
    let opt = PathBuf::from(format!("{prefix}/opt/{formula}/{tail}"));
    opt.exists().then_some(opt)
}

/// Gateway options are stored in the login item; a token there is readable by other programs
/// running as you, so say so instead of silently writing it to disk.
fn warn_about_secrets(args: &[String]) {
    if args.iter().any(|a| a.starts_with("--admin-token") || a.starts_with("--admin-read-token")) {
        eprintln!("note: the admin token is saved in the login item file, readable by programs running as you");
    }
}

// Built on every OS so each platform's file format is unit-tested everywhere; only one is used per build.
#[cfg_attr(not(test), allow(dead_code))]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

// Built on every OS so each platform's file format is unit-tested everywhere; only one is used per build.
#[cfg_attr(not(test), allow(dead_code))]
/// LaunchAgent plist: runs at login and restarts the gateway if it crashes (not after a clean exit).
fn launchd_plist(exe: &Path, args: &[String], log: &Path) -> String {
    let mut program = vec![exe.display().to_string()];
    program.extend(args.iter().cloned());
    let items: String = program.iter().map(|a| format!("    <string>{}</string>\n", xml_escape(a))).collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{items}  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        log = xml_escape(&log.display().to_string())
    )
}

// Built on every OS so each platform's file format is unit-tested everywhere; only one is used per build.
#[cfg_attr(not(test), allow(dead_code))]
/// systemd quoting: wrap every word in double quotes and escape `\`, `"` and `%` (specifier).
fn systemd_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%"))
}

// Built on every OS so each platform's file format is unit-tested everywhere; only one is used per build.
#[cfg_attr(not(test), allow(dead_code))]
fn systemd_unit(exe: &Path, args: &[String]) -> String {
    let mut words = vec![systemd_quote(&exe.display().to_string())];
    words.extend(args.iter().map(|a| systemd_quote(a)));
    format!(
        "[Unit]\nDescription=eay-gateway (local AI gateway)\nAfter=network-online.target\n\n\
         [Service]\nExecStart={}\nRestart=on-failure\nRestartSec=5\n\n\
         [Install]\nWantedBy=default.target\n",
        words.join(" ")
    )
}

// Built on every OS so each platform's file format is unit-tested everywhere; only one is used per build.
#[cfg_attr(not(test), allow(dead_code))]
/// Windows command-line quoting for one argument (CommandLineToArgvW rules).
fn windows_quote(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in a.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

fn sh(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd).args(args).output().map_err(|e| format!("cannot run {cmd}: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_string();
    if out.status.success() {
        Ok(text)
    } else {
        Err(format!("{cmd} {}: {text}", args.join(" ")))
    }
}

// ---- macOS -------------------------------------------------------------

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf, String> {
    Ok(home()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn uid() -> Result<String, String> {
    sh("id", &["-u"])
}

#[cfg(target_os = "macos")]
fn install(args: &[String]) -> Result<String, String> {
    warn_about_secrets(args);
    let path = plist_path()?;
    let log = home()?.join("Library/Logs/eay-gateway.log");
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(log.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    let domain = format!("gui/{}", uid()?);
    // Replace an older install so changed options take effect.
    let _ = sh("launchctl", &["bootout", &format!("{domain}/{LABEL}")]);
    std::fs::write(&path, launchd_plist(&current_exe()?, args, &log)).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    sh("launchctl", &["bootstrap", &domain, &path.display().to_string()])?;
    Ok(format!(
        "Installed: eay-gateway starts at login and is running now.\n  login item: {}\n  log:        {}",
        path.display(),
        log.display()
    ))
}

#[cfg(target_os = "macos")]
fn uninstall() -> Result<String, String> {
    let path = plist_path()?;
    let _ = sh("launchctl", &["bootout", &format!("gui/{}/{LABEL}", uid()?)]);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok("Uninstalled: eay-gateway no longer starts at login.".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("Not installed.".into()),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

#[cfg(target_os = "macos")]
fn status() -> Result<String, String> {
    let path = plist_path()?;
    if !path.exists() {
        return Ok("Not installed. Run `eay-gateway service install`.".into());
    }
    let running = sh("launchctl", &["print", &format!("gui/{}/{LABEL}", uid()?)]).map(|o| o.contains("state = running")).unwrap_or(false);
    Ok(format!("Installed ({}), {}.", path.display(), if running { "running" } else { "not running" }))
}

// ---- Linux ---------------------------------------------------------------

#[cfg(target_os = "linux")]
fn unit_path() -> Result<PathBuf, String> {
    let base = std::env::var("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|_| home().map(|h| h.join(".config")))?;
    Ok(base.join("systemd/user").join(UNIT))
}

#[cfg(target_os = "linux")]
fn install(args: &[String]) -> Result<String, String> {
    warn_about_secrets(args);
    let path = unit_path()?;
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    std::fs::write(&path, systemd_unit(&current_exe()?, args)).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    sh("systemctl", &["--user", "daemon-reload"])?;
    sh("systemctl", &["--user", "enable", "--now", UNIT])?;
    sh("systemctl", &["--user", "restart", UNIT])?;
    Ok(format!(
        "Installed: eay-gateway starts at login and is running now.\n  unit: {}\n  logs: journalctl --user -u {UNIT}\n  \
         To keep it running when you are logged out: loginctl enable-linger $USER",
        path.display()
    ))
}

#[cfg(target_os = "linux")]
fn uninstall() -> Result<String, String> {
    let path = unit_path()?;
    if !path.exists() {
        return Ok("Not installed.".into());
    }
    let _ = sh("systemctl", &["--user", "disable", "--now", UNIT]);
    std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    let _ = sh("systemctl", &["--user", "daemon-reload"]);
    Ok("Uninstalled: eay-gateway no longer starts at login.".into())
}

#[cfg(target_os = "linux")]
fn status() -> Result<String, String> {
    let path = unit_path()?;
    if !path.exists() {
        return Ok("Not installed. Run `eay-gateway service install`.".into());
    }
    let state = sh("systemctl", &["--user", "is-active", UNIT]).unwrap_or_else(|e| e);
    Ok(format!("Installed ({}), {}.", path.display(), state.lines().last().unwrap_or("unknown")))
}

// ---- Windows ---------------------------------------------------------------

#[cfg(windows)]
fn install(args: &[String]) -> Result<String, String> {
    warn_about_secrets(args);
    let mut words = vec![windows_quote(&current_exe()?.display().to_string())];
    words.extend(args.iter().map(|a| windows_quote(a)));
    let tr = words.join(" ");
    if tr.len() > 261 {
        return Err("the command is longer than Task Scheduler allows (261 characters); use fewer options or set them as EAY_GATEWAY_* environment variables".into());
    }
    sh("schtasks", &["/Create", "/F", "/SC", "ONLOGON", "/RL", "LIMITED", "/TN", TASK, "/TR", &tr])?;
    let _ = sh("schtasks", &["/Run", "/TN", TASK]);
    Ok(format!("Installed: eay-gateway starts at logon and is running now (task \"{TASK}\")."))
}

#[cfg(windows)]
fn uninstall() -> Result<String, String> {
    if sh("schtasks", &["/Query", "/TN", TASK]).is_err() {
        return Ok("Not installed.".into());
    }
    let _ = sh("schtasks", &["/End", "/TN", TASK]);
    sh("schtasks", &["/Delete", "/F", "/TN", TASK])?;
    Ok("Uninstalled: eay-gateway no longer starts at logon.".into())
}

#[cfg(windows)]
fn status() -> Result<String, String> {
    match sh("schtasks", &["/Query", "/TN", TASK, "/FO", "LIST"]) {
        Ok(out) => {
            let state = out.lines().find(|l| l.trim_start().starts_with("Status:")).unwrap_or("Status: unknown").trim();
            Ok(format!("Installed (task \"{TASK}\"), {state}."))
        }
        Err(_) => Ok("Not installed. Run `eay-gateway service install`.".into()),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn install(_: &[String]) -> Result<String, String> {
    Err("`service` is supported on macOS, Linux and Windows".into())
}
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn uninstall() -> Result<String, String> {
    install(&[])
}
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn status() -> Result<String, String> {
    install(&[])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn launchd_plist_escapes_and_keeps_every_argument() {
        let p = launchd_plist(
            Path::new("/opt/x/eay-gateway"),
            &args(&["--console-origin", "https://a.io/?a=1&b=<2>"]),
            Path::new("/tmp/l.log"),
        );
        assert!(p.contains("<string>/opt/x/eay-gateway</string>"));
        assert!(p.contains("<string>https://a.io/?a=1&amp;b=&lt;2&gt;</string>"));
        assert!(p.contains(&format!("<string>{LABEL}</string>")));
        assert!(p.contains("<key>SuccessfulExit</key>"));
    }

    #[test]
    fn systemd_unit_quotes_spaces_quotes_and_percent() {
        let u = systemd_unit(Path::new("/home/a b/eay-gateway"), &args(&["--admin-token", "x\"y%z"]));
        assert!(u.contains(r#"ExecStart="/home/a b/eay-gateway" "--admin-token" "x\"y%%z""#), "{u}");
        assert!(u.contains("Restart=on-failure"));
    }

    #[test]
    fn windows_quoting_follows_argv_rules() {
        assert_eq!(windows_quote("plain"), "plain");
        assert_eq!(windows_quote(r"C:\Program Files\eay.exe"), r#""C:\Program Files\eay.exe""#);
        assert_eq!(windows_quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(windows_quote(r"ends\ "), r#""ends\ ""#);
        assert_eq!(windows_quote(r"trail\"), r"trail\");
        assert_eq!(windows_quote(r"sp ace\"), r#""sp ace\\""#);
        assert_eq!(windows_quote(""), r#""""#);
    }

    #[test]
    fn homebrew_cellar_path_maps_to_the_stable_opt_link() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        let opt = dir.path().join("opt/eay-gateway/bin");
        std::fs::create_dir_all(&opt).unwrap();
        std::fs::write(opt.join("eay-gateway"), "").unwrap();
        let cellar = PathBuf::from(format!("{root}/Cellar/eay-gateway/0.3.0/bin/eay-gateway"));
        assert_eq!(stable_brew_path(&cellar), Some(opt.join("eay-gateway")));
        assert_eq!(stable_brew_path(Path::new("/usr/local/bin/eay-gateway")), None);
    }

    #[test]
    fn unknown_action_prints_usage_and_fails() {
        assert_eq!(run(&args(&["bogus"])), 2);
        assert_eq!(run(&args(&["--help"])), 0);
    }
}
