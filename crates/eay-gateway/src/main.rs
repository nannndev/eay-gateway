use axum::{routing::get, Json};
use eay_gateway::core::{GatewayEngine, GatewayStore};
use eay_gateway::gateway_router;
use serde_json::{json, Value};
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

struct Config {
    host: String,
    port: u16,
    data_dir: PathBuf,
}

/// Per-user data folder, plus the folder older versions (Notchi Gateway) used.
fn data_dirs() -> (PathBuf, Option<PathBuf>) {
    #[cfg(target_os = "windows")]
    if let Ok(appdata) = env::var("APPDATA").filter(|a| !a.is_empty()) {
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

/// `EAY_GATEWAY_*`, falling back to the older `NOTCHI_GATEWAY_*` names.
fn env_var(name: &str) -> Option<String> {
    env::var(format!("EAY_GATEWAY_{name}")).or_else(|_| env::var(format!("NOTCHI_GATEWAY_{name}"))).ok().filter(|v| !v.is_empty())
}

fn parse_args() -> Result<Config, String> {
    let mut host = env_var("HOST").unwrap_or_else(|| "127.0.0.1".to_string());
    let mut port: u16 = env_var("PORT").and_then(|v| v.parse().ok()).unwrap_or(20129);
    let mut data_dir = env_var("DATA_DIR").map(PathBuf::from).unwrap_or_else(default_data_dir);

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "eay-gateway {}\n\nUSAGE:\n  eay-gateway [OPTIONS]\n\nOPTIONS:\n  --host <HOST>          Bind host (default: 127.0.0.1, env: EAY_GATEWAY_HOST)\n  --port <PORT>          Bind port (default: 20129, env: EAY_GATEWAY_PORT)\n  --data-dir <PATH>      Data directory (default: ~/.eay-gateway, env: EAY_GATEWAY_DATA_DIR)\n  -h, --help             Print help",
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
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Config { host, port, data_dir })
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

#[tokio::main]
async fn main() {
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

    let store = match GatewayStore::new(&cfg.data_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to initialize gateway store at {}: {e}", cfg.data_dir.display());
            std::process::exit(1);
        }
    };

    let engine = GatewayEngine::new(store);
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
    let app = gateway_router(engine).route("/health", get(root_health));

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind {addr}: {e}");
            std::process::exit(1);
        }
    };

    eprintln!("eay-gateway v{} listening on http://{} (data: {})", env!("CARGO_PKG_VERSION"), addr, cfg.data_dir.display());

    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}
