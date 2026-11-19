mod service;

use axum::{routing::get, Json};
use eay_gateway::core::{GatewayEngine, GatewayStore};
use eay_gateway::{gateway_router, gateway_service};
use serde_json::{json, Value};
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

struct Config {
    host: String,
    port: u16,
    data_dir: PathBuf,
    /// Exit after this long without HTTP requests; None runs until stopped.
    idle_exit: Option<std::time::Duration>,
}

/// Per-user data folder, plus the folder older versions (Notchi Gateway) used.
fn data_dirs() -> (PathBuf, Option<PathBuf>) {
    #[cfg(target_os = "windows")]
    if let Some(appdata) = env::var("APPDATA").ok().filter(|a| !a.is_empty()) {
        let base = PathBuf::from(appdata);
        return (base.join("eay-gateway"), Some(base.join("notchi-gateway")));
    }
    match env::var("HOME").or_else(|_| env::var("USERPROFILE")).ok().filter(|h| !h.is_empty()) {
        Some(home) => {
            let home = PathBuf::from(home);
            (home.join(".eay-gateway"), Some(home.join(".notchi-gateway")))
        }
        None => (PathBuf::from(".eay-gateway"), None),
    }
}

/// The default data folder; on first run it takes over the old Notchi Gateway folder so
/// connected accounts and keys survive the rename.
fn default_data_dir() -> PathBuf {
    let (dir, legacy) = data_dirs();
    if let Some(old) = legacy.filter(|old| !dir.exists() && old.join("gateway.db").exists()) {
        if let Err(e) = std::fs::rename(&old, &dir) {
            eprintln!("could not move {} to {}: {e}; using the old folder", old.display(), dir.display());
            return old;
        }
    }
    dir
}

/// Exclusive lock on `<data>/gateway.lock`; fails if another gateway already uses this folder.
fn lock_data_dir(dir: &std::path::Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create data folder {}: {e}", dir.display()))?;
    let path = dir.join("gateway.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err(format!("another eay-gateway is already using {}; use that one or pass a different --data-dir", dir.display()))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(format!("cannot lock {}: {e}", path.display())),
    }
}

/// `EAY_GATEWAY_*`, falling back to the older `NOTCHI_GATEWAY_*` names.
fn env_var(name: &str) -> Option<String> {
    env::var(format!("EAY_GATEWAY_{name}")).or_else(|_| env::var(format!("NOTCHI_GATEWAY_{name}"))).ok().filter(|v| !v.is_empty())
}

fn parse_args() -> Result<Config, String> {
    let mut host = env_var("HOST").unwrap_or_else(|| "127.0.0.1".to_string());
    let mut port: u16 = env_var("PORT").and_then(|v| v.parse().ok()).unwrap_or(20129);
    let mut data_dir = env_var("DATA_DIR").map(PathBuf::from).unwrap_or_else(default_data_dir);
    let mut idle_exit = None;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "eay-gateway {}\n\nUSAGE:\n  eay-gateway [OPTIONS]\n\nOPTIONS:\n  --host <HOST>          Bind host (default: 127.0.0.1, env: EAY_GATEWAY_HOST)\n  --port <PORT>          Bind port (default: 20129, env: EAY_GATEWAY_PORT)\n  --data-dir <PATH>      Data directory (default: ~/.eay-gateway, env: EAY_GATEWAY_DATA_DIR)\n  --admin-token <T>      Full admin access token (env: EAY_GATEWAY_ADMIN_TOKEN)\n  --admin-read-token <T> Read-only (GET) admin token (env: EAY_GATEWAY_ADMIN_READ_TOKEN)\n  --console-origin <O>   Browser origin(s) allowed to call the gateway, comma-separated, e.g. the hosted\n                         eay-console (env: EAY_GATEWAY_CONSOLE_ORIGINS). http://localhost:<port> is always allowed\n  --idle-exit <MIN>      Exit after MIN minutes without requests (for apps that share one gateway)\n  -V, --version          Print version\n  -h, --help             Print help\n\nCOMMANDS:\n  service install [OPTIONS]  Start at login with these options (service uninstall | status)",
                    env!("CARGO_PKG_VERSION")
                );
                std::process::exit(0);
            }
            "--host" => {
                host = args.next().ok_or("--host requires a value")?;
            }
            "--port" => {
                let val = args.next().ok_or("--port requires a value")?;
                port = val.parse().map_err(|_| format!("invalid --port value: {val}"))?;
            }
            "--data-dir" => {
                let val = args.next().ok_or("--data-dir requires a value")?;
                data_dir = PathBuf::from(val);
            }
            "--admin-token" => {
                let val = args.next().ok_or("--admin-token requires a value")?;
                std::env::set_var("EAY_GATEWAY_ADMIN_TOKEN", val);
            }
            "--admin-read-token" => {
                let val = args.next().ok_or("--admin-read-token requires a value")?;
                std::env::set_var("EAY_GATEWAY_ADMIN_READ_TOKEN", val);
            }
            "--console-origin" => {
                let val = args.next().ok_or("--console-origin requires an origin, e.g. https://nannndev.github.io")?;
                std::env::set_var("EAY_GATEWAY_CONSOLE_ORIGINS", val);
            }
            "--idle-exit" => {
                let val = args.next().ok_or("--idle-exit requires minutes")?;
                let mins: u64 = val.parse().map_err(|_| format!("invalid --idle-exit value: {val}"))?;
                idle_exit = (mins > 0).then(|| std::time::Duration::from_secs(mins * 60));
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Config { host, port, data_dir, idle_exit })
}

async fn root_health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "eay-gateway",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            sig.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

/// Resolves once no request has arrived for `limit`.
async fn idle_timeout(last_request: std::sync::Arc<std::sync::Mutex<std::time::Instant>>, limit: std::time::Duration) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        let idle = last_request.lock().map(|t| t.elapsed()).unwrap_or_default();
        if idle >= limit {
            eprintln!("no requests for {} min; exiting", limit.as_secs() / 60);
            return;
        }
    }
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("service") {
        std::process::exit(service::run(&argv[1..]));
    }
    if matches!(argv.first().map(String::as_str), Some("-V" | "--version")) {
        println!("eay-gateway {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let cfg = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    let addr: SocketAddr = match format!("{}:{}", cfg.host, cfg.port).parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("invalid bind address {}:{}: {e}", cfg.host, cfg.port);
            std::process::exit(2);
        }
    };

    // Two gateways sharing a data folder would refresh the same OAuth tokens and invalidate each
    // other's; hold an exclusive lock for the life of the process (released by the OS on exit).
    let _lock = match lock_data_dir(&cfg.data_dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(3);
        }
    };

    let store = match GatewayStore::new(&cfg.data_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to initialize gateway store at {}: {e}", cfg.data_dir.display());
            std::process::exit(1);
        }
    };

    let engine = GatewayEngine::new(store);
    engine.restore_subagent_history();
    {
        // Keep provider model lists fresh: check hourly, refetch lists older than a day.
        let engine = engine.clone();
        tokio::spawn(async move {
            loop {
                eay_gateway::core::models::sync_stale_models(&engine).await;
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        });
    }
    // Apps that start the gateway on demand pass --idle-exit, so it outlives any one app but
    // still goes away once nobody uses it.
    let last_request = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let touch = last_request.clone();
    let app = gateway_router(engine).route("/health", get(root_health)).layer(axum::middleware::from_fn(
        move |req: axum::extract::Request, next: axum::middleware::Next| {
            if let Ok(mut t) = touch.lock() {
                *t = std::time::Instant::now();
            }
            next.run(req)
        },
    ));

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind {addr}: {e}");
            std::process::exit(1);
        }
    };

    eprintln!("eay-gateway v{} listening on http://{} (data: {})", env!("CARGO_PKG_VERSION"), addr, cfg.data_dir.display());

    if let Err(e) = axum::serve(listener, tower::make::Shared::new(gateway_service(app)))
        .with_graceful_shutdown(async move {
            match cfg.idle_exit {
                Some(limit) => tokio::select! { _ = shutdown_signal() => {}, _ = idle_timeout(last_request, limit) => {} },
                None => shutdown_signal().await,
            }
        })
        .await
    {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::lock_data_dir;

    #[test]
    fn a_second_lock_on_the_same_data_dir_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let _first = lock_data_dir(dir.path()).unwrap();
        assert!(lock_data_dir(dir.path()).is_err());
    }

    #[test]
    fn the_lock_is_free_again_once_released() {
        let dir = tempfile::tempdir().unwrap();
        drop(lock_data_dir(dir.path()).unwrap());
        assert!(lock_data_dir(dir.path()).is_ok());
    }
}
