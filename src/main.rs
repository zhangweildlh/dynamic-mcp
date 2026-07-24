mod auth;
mod cli;
mod config;
mod http;
mod proxy;
mod server;
mod singleton;
mod watcher;

use anyhow::Result;
use clap::{Parser, Subcommand};
use proxy::ModularMcpClient;
use server::ModularMcpServer;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::RwLock;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;
use watcher::ConfigWatcher;

use http::server_handler::HttpFacadeHandler;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use singleton::{SingletonResult, StartMode};
use std::net::SocketAddr;
use std::time::Duration;
use tower_http::cors::CorsLayer;

#[derive(Parser)]
#[command(name = "dynamic-mcp")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Dynamic MCP Proxy Server - Reduce context overhead with on-demand tool loading")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Configuration file path (when running as server without subcommand)
    config_path: Option<String>,

    /// Transport mode: stdio, http, or both
    #[arg(long, value_enum, default_value = "stdio")]
    transport: TransportMode,

    /// Full HTTP endpoint `host:port/path` to bind when transport includes http.
    /// IPv6 uses `[host]:port/path`. Defaults to `127.0.0.1:8082/dynamic-mcp`.
    #[arg(long, default_value = "127.0.0.1:8082/dynamic-mcp")]
    http_endpoint: String,

    /// When transport is `http`, allow a later `both` instance on the same
    /// endpoint to run alongside (stdio only) instead of being evicted. Has no
    /// effect with `--transport both` or `--transport stdio`.
    #[arg(long, default_value_t = false)]
    no_evict: bool,

    /// Log level: trace/debug/info/warn/error. Invalid value falls back to warn.
    /// When set: every mode writes a log file next to the executable
    /// (falls back to data_local_dir/dynamic-mcp if not writable);
    /// http mode also prints to stderr. stdio/both modes stay silent on
    /// stderr to protect the JSON-RPC channel.
    #[arg(long, value_name = "LEVEL")]
    log: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Import MCP config from AI coding tools to dynamic-mcp format
    Import {
        /// Tool name: cursor, opencode, claude-desktop, claude, vscode, cline, kilocode, codex, gemini, antigravity
        tool_name: String,

        /// Use global/user config instead of project config
        #[arg(short, long)]
        global: bool,

        /// Force overwrite existing output file without prompting
        #[arg(short, long)]
        force: bool,

        /// Output path for dynamic-mcp.json
        #[arg(short, long, default_value = "dynamic-mcp.json")]
        output: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum TransportMode {
    /// Standard input/output JSON-RPC (default)
    #[value(name = "stdio")]
    Stdio,
    /// Streamable HTTP MCP endpoint only
    #[value(name = "http")]
    Http,
    /// Both stdio and Streamable HTTP simultaneously
    #[value(name = "both")]
    Both,
}

fn get_config_path(cli_arg: Option<String>) -> Option<(String, &'static str)> {
    if let Some(path) = cli_arg {
        Some((path, "command line argument"))
    } else if let Ok(path) = std::env::var("DYNAMIC_MCP_CONFIG") {
        if path.is_empty() {
            None
        } else {
            Some((path, "DYNAMIC_MCP_CONFIG environment variable"))
        }
    } else {
        None
    }
}

/// Parse a `--log` level string into a `LevelFilter`, falling back to WARN.
fn parse_level(s: &str) -> LevelFilter {
    match s.to_ascii_lowercase().as_str() {
        "trace" => LevelFilter::TRACE,
        "debug" => LevelFilter::DEBUG,
        "info" => LevelFilter::INFO,
        "warn" => LevelFilter::WARN,
        "error" => LevelFilter::ERROR,
        _ => LevelFilter::WARN,
    }
}

/// Resolve the directory for log/tool-dump files: next to the executable,
/// falling back to `data_local_dir/dynamic-mcp` when the exe dir is missing
/// or not writable (e.g. Program Files under restrictive ACLs / EDR).
fn log_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.exists() {
                // exists() ≠ writable: probe with a temp file.
                let probe = dir.join(format!(".dynamic-mcp-writable-{}.tmp", std::process::id()));
                if std::fs::File::create(&probe).is_ok() {
                    let _ = std::fs::remove_file(&probe);
                    return dir.to_path_buf();
                }
            }
        }
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("dynamic-mcp")
}

/// Delete `dynamic-*.log` files whose mtime is older than `max_age`,
/// skipping the currently-open log file. Failures are silently ignored.
fn cleanup_old_logs(dir: &Path, current: &Path, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let cutoff = std::time::SystemTime::now()
        .checked_sub(max_age)
        .unwrap_or(std::time::UNIX_EPOCH);
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("skip unreadable log entry: {}", e);
                continue;
            }
        };
        let p = entry.path();
        if p == *current {
            continue;
        }
        if p.extension().and_then(|s| s.to_str()) != Some("log") {
            continue;
        }
        if p.file_name()
            .and_then(|s| s.to_str())
            .map(|n| !n.starts_with("dynamic-"))
            .unwrap_or(true)
        {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&p) {
            if let Ok(m) = meta.modified() {
                if m < cutoff {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Import {
            tool_name,
            global,
            force,
            output,
        }) => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
                )
                .init();
            cli::import::run_import_from_tool(&tool_name, global, force, &output).await
        }
        None => {
            // Disable all logging for stdio mode to avoid corrupting JSON-RPC communication
            // Logging would write to stderr which interferes with the MCP protocol

            let (config_path, config_source) =
                get_config_path(cli.config_path).unwrap_or_else(|| {
                    eprintln!("Error: No configuration file specified");
                    eprintln!();
                    eprintln!("Usage: dynamic-mcp <config-file>");
                    eprintln!("   or: DYNAMIC_MCP_CONFIG=<config-file> dynamic-mcp");
                    eprintln!();
                    eprintln!("Example: dynamic-mcp config.example.json");
                    eprintln!("     or: DYNAMIC_MCP_CONFIG=config.example.json dynamic-mcp");
                    std::process::exit(1);
                });

            // `--no-evict` only makes sense for a pure `http` instance: it tells a
            // later `both` not to evict this http. Reject it otherwise.
            if cli.no_evict && cli.transport != TransportMode::Http {
                eprintln!(
                    "Error: --no-evict is only valid with --transport http (a pure-http instance)."
                );
                std::process::exit(1);
            }

            run_server(
                config_path,
                config_source,
                cli.transport,
                cli.http_endpoint,
                cli.no_evict,
                cli.log,
            )
            .await
        }
    }
}

async fn run_server(
    config_path: String,
    config_source: &str,
    transport: TransportMode,
    http_endpoint: String,
    no_evict: bool,
    log: Option<String>,
) -> Result<()> {
    // ---- Logging (v1.8.2 hybrid scheme) ----
    // No `--log`: http mode defaults to WARN on stderr; stdio/both silent; no file.
    // With `--log <LEVEL>`: every mode writes a log file; http also prints to
    // stderr. stdio/both never print to stderr (protect JSON-RPC). File path =
    // next to the executable, or data_local_dir/dynamic-mcp if not writable.
    let log_filter = log.as_deref().map(parse_level).unwrap_or(LevelFilter::WARN);
    let mut layers: Vec<
        Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>,
    > = Vec::new();
    let mut log_path: Option<PathBuf> = None;

    if log.is_some() {
        let dir = log_dir();
        let _ = std::fs::create_dir_all(&dir);
        let ts = chrono::Local::now().format("%Y%m%d-%H%M%S%3f").to_string();
        let path = dir.join(format!("dynamic-{}-{}.log", std::process::id(), ts));
        if let Ok(file) = std::fs::File::create(&path) {
            log_path = Some(path);
            layers.push(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(Mutex::new(file))
                    .with_filter(log_filter)
                    .boxed(),
            );
        }
        // Cleanup: reuse the dir resolved above (do NOT re-call log_dir()).
        if let Some(ref p) = log_path {
            let cleanup_dir = dir.clone();
            let cleanup_path = p.clone();
            tokio::spawn(async move {
                cleanup_old_logs(
                    &cleanup_dir,
                    &cleanup_path,
                    std::time::Duration::from_secs(72 * 3600),
                );
            });
        }
    }

    if matches!(transport, TransportMode::Http) {
        layers.push(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(log_filter)
                .boxed(),
        );
    }

    let _ = tracing_subscriber::registry().with(layers).try_init();

    // Early singleton / double-launch detection. This may:
    //  - SelfTerminate a redundant `http` after 8s (A1/A3),
    //  - Evict an old `http` and delay our own HTTP by 8s (B1),
    //  - Keep stdio only (B2/B3), or proceed normally.
    let (ep_host, ep_port, ep_path) = parse_http_endpoint(&http_endpoint)
        .map_err(|e| anyhow::anyhow!("无效的 --http-endpoint 参数：{e}"))?;
    let canonical = canonical_endpoint(&ep_host, ep_port, &ep_path);
    // IPv6 绑定需要方括号形式；锁 key 用裸 host（见 D4）。
    let bind_host = if ep_host.contains(':') {
        format!("[{}]", ep_host)
    } else {
        ep_host.clone()
    };
    let display = format!("{}:{}", bind_host, ep_port);
    let SingletonResult { mode, guard, popup } =
        singleton::check_singleton(transport, &canonical, &display, no_evict).await;
    // Hold the lock guard for the process lifetime so our lock file is removed on
    // exit (unless a newer primary has overwritten it first).
    let _singleton_guard = guard;
    popup.emit();

    if matches!(mode, StartMode::SelfTerminate) {
        // Redundant http: the popup is already shown. Exit after 8s so the user
        // can read it. Nothing else to do.
        tokio::time::sleep(Duration::from_secs(8)).await;
        std::process::exit(0);
    }

    tracing::info!(
        "Starting dynamic-mcp server with config: {} (from {})",
        &config_path,
        config_source
    );

    let config_path_buf = std::path::Path::new(&config_path).canonicalize()?;
    let (config_watcher, mut reload_rx) = ConfigWatcher::new(&config_path_buf)?;

    let client = Arc::new(RwLock::new(ModularMcpClient::new()));

    // Validate initial config
    config::load_config(&config_path).await?;

    // Initial load - BACKGROUND connect with a short grace window.
    // Per-group connects run in parallel via tokio::spawn (non-blocking, exactly
    // as upstream does). We deliberately do NOT await all handles to completion,
    // because that could take ~10s and would exceed the MCP connector's
    // process-launch timeout, causing the connector to kill & restart dmcp in a
    // loop (observed crash-restart loop with old B1). Instead we only wait up to
    // a short grace period (3s) before entering run_stdio; any group whose
    // connect is still in flight keeps running in the background (tokio::spawn
    // tasks are NOT cancelled when their JoinHandle is dropped) and becomes
    // ready shortly after. The rare first request that arrives before a slow
    // group is connected is handled by the normal "Group not found" path, which
    // is mitigated at the config layer (A2) and by the mimo_mcp.py process-tree
    // cleanup. (B1: avoid startup blocking that triggered the launch-timeout
    // restart loop.)
    let client_init = client.clone();
    let config_path_init = config_path.clone();
    if let Ok(config) = config::load_config(&config_path_init).await {
        let servers: Vec<_> = config
            .mcp_servers
            .into_iter()
            .filter(|(_, server_config)| {
                if !server_config.is_enabled() {
                    tracing::info!("⊘ Server is disabled, skipping connection");
                }
                server_config.is_enabled()
            })
            .collect();

        let handles: Vec<_> = servers
            .into_iter()
            .map(|(group_name, server_config)| {
                let client = client_init.clone();
                tokio::spawn(async move {
                    let res = {
                        let mut client_lock = client.write().await;
                        client_lock
                            .connect(group_name.clone(), server_config.clone())
                            .await
                    };
                    match res {
                        Ok(_) => Ok(group_name),
                        Err(e) => {
                            let mut client_lock = client.write().await;
                            client_lock.record_failed_connection(
                                group_name.clone(),
                                server_config,
                                e,
                            );
                            Err(group_name)
                        }
                    }
                })
            })
            .collect();

        // 短宽限：最多等待 3 秒让 group 连接，超时即放行进 run_stdio。
        // 未完成的连接任务仍在后台继续运行（tokio::spawn 的任务不会因
        // JoinHandle 被 drop 而取消），待其完成后即可正常服务。
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            for handle in handles {
                let _ = handle.await;
            }
        })
        .await;
    }

    // Spawn periodic retry handler for failed connections
    let client_retry = client.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        interval.tick().await;

        loop {
            interval.tick().await;
            let mut client_lock = client_retry.write().await;
            let failed = client_lock.list_failed_groups();

            if !failed.is_empty() {
                tracing::debug!("Periodic retry check: {} failed groups", failed.len());
                let retried = client_lock.retry_failed_connections().await;
                if !retried.is_empty() {
                    tracing::info!("✅ Periodic retry reconnected: {}", retried.join(", "));
                }
            }
        }
    });

    // Spawn config reload handler
    let client_clone = client.clone();
    let config_path_clone = config_path.clone();
    tokio::spawn(async move {
        while reload_rx.recv().await.is_some() {
            tracing::info!("Config file changed, reloading...");

            match config::load_config(&config_path_clone).await {
                Ok(new_config) => {
                    let mut client_lock = client_clone.write().await;

                    // Disconnect all existing connections
                    if let Err(e) = client_lock.disconnect_all().await {
                        tracing::error!("Failed to disconnect all groups: {}", e);
                    }

                    // Reconnect with new config
                    for (group_name, server_config) in new_config.mcp_servers {
                        if !server_config.is_enabled() {
                            tracing::info!(
                                "⊘ Server is disabled, skipping connection: {}",
                                group_name
                            );
                            continue;
                        }

                        match client_lock
                            .connect(group_name.clone(), server_config.clone())
                            .await
                        {
                            Ok(_) => {
                                tracing::info!(
                                    "✅ Successfully reconnected to MCP group: {}",
                                    group_name
                                );
                            }
                            Err(e) => {
                                tracing::error!("❌ Failed to reconnect to {}: {}", group_name, e);
                                client_lock.record_failed_connection(group_name, server_config, e);
                            }
                        }
                    }

                    let groups = client_lock.list_groups();
                    let failed = client_lock.list_failed_groups();

                    if failed.is_empty() {
                        tracing::info!(
                            "✅ Config reload complete: {} groups connected",
                            groups.len()
                        );
                    } else {
                        tracing::warn!(
                            "⚠️ Config reload complete with errors. success_groups=[{}], failed_groups=[{}]",
                            groups.iter().map(|g| &g.name).cloned().collect::<Vec<_>>().join(", "),
                            failed.iter().map(|g| &g.name).cloned().collect::<Vec<_>>().join(", ")
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("❌ Failed to reload config: {}", e);
                }
            }
        }
    });

    let name = env!("CARGO_PKG_NAME").to_string();
    let version = env!("CARGO_PKG_VERSION").to_string();

    let stdio_enabled = matches!(transport, TransportMode::Stdio | TransportMode::Both);
    let mut http_enabled = matches!(transport, TransportMode::Http | TransportMode::Both);

    // The singleton decision may force HTTP off (B2/B3: keep stdio only).
    if matches!(mode, StartMode::StdioOnly) {
        http_enabled = false;
    }

    if http_enabled {
        let client_http = client.clone();
        let name_http = name.clone();
        let version_http = version.clone();
        let host = bind_host.clone();
        let port = ep_port;
        let path = ep_path.clone();

        // B1: the `both` evicted an old `http` and HTTP must start 8s later so the
        // port (TIME_WAIT) is free. Otherwise start immediately.
        let delay = matches!(mode, StartMode::DelayHttpThenNormal);
        tokio::spawn(async move {
            if delay {
                tokio::time::sleep(Duration::from_secs(8)).await;
            }
            start_http_server(client_http, name_http, version_http, host, port, path).await;
        });
    }

    tracing::info!("MCP server initialized (transport={:?})", transport);

    // Keep watcher alive
    std::mem::forget(config_watcher);

    // Set up signal handler for graceful shutdown
    let client_for_shutdown = client.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Received shutdown signal, disconnecting all servers...");
        let mut client_lock = client_for_shutdown.write().await;
        let _ = client_lock.disconnect_all().await;
        std::process::exit(0);
    });

    let result = if stdio_enabled {
        let server = ModularMcpServer::new(client.clone(), name.clone(), version.clone());
        server.run_stdio().await
    } else {
        // HTTP-only mode: the spawned HTTP task keeps serving until Ctrl-C,
        // which the shutdown handler above uses to exit the process.
        tracing::info!("stdio transport disabled; running HTTP-only. Press Ctrl-C to stop.");
        tokio::signal::ctrl_c().await.ok();
        let mut client_lock = client.write().await;
        let _ = client_lock.disconnect_all().await;
        Ok(())
    };

    // Cleanup on normal exit (stdin closed or signal in http-only mode)
    {
        let mut client_lock = client.write().await;
        let _ = client_lock.disconnect_all().await;
    }

    result
}

/// Start the Streamable HTTP MCP server. Binds with `SO_REUSEADDR` and retries
/// for up to ~10s so an eviction (B1) can take over the port even if it is still
/// in `TIME_WAIT`.
async fn start_http_server(
    client: Arc<RwLock<ModularMcpClient>>,
    name: String,
    version: String,
    host: String,
    port: u16,
    path: String,
) {
    let factory = move || {
        Ok::<_, std::io::Error>(HttpFacadeHandler::new(
            client.clone(),
            name.clone(),
            version.clone(),
        ))
    };

    let service = StreamableHttpService::new(
        factory,
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    let app = axum::Router::new()
        .nest_service(&path, service)
        .layer(CorsLayer::permissive());

    let addr: SocketAddr = match format!("{}:{}", host, port).parse() {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("Invalid HTTP listen address {}:{}: {}", host, port, e);
            return;
        }
    };

    match bind_with_retry(&addr).await {
        Ok(listener) => {
            tracing::info!(
                "MCP Streamable HTTP server listening on http://{}{}",
                addr,
                path
            );
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("Streamable HTTP server error: {}", e);
            }
        }
        Err(e) => {
            tracing::error!(
                "Failed to bind Streamable HTTP listener on {} after retries: {}",
                addr,
                e
            );
        }
    }
}

/// Try to bind `addr`, retrying for ~10s. Enables `SO_REUSEADDR` first so a port
/// left in `TIME_WAIT` (common right after an eviction) can be reused.
async fn bind_with_retry(addr: &SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    // The initial `None` is only ever observed if the deadline is already past at
    // the very first iteration; clippy otherwise flags it as an unused assignment.
    #[allow(unused_assignments)]
    let mut last_err = None;
    loop {
        let socket = if addr.is_ipv4() {
            tokio::net::TcpSocket::new_v4()
        } else {
            tokio::net::TcpSocket::new_v6()
        };
        let socket = match socket {
            Ok(s) => s,
            Err(e) => {
                last_err = Some(e);
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        let _ = socket.set_reuseaddr(true);
        match socket.bind(*addr) {
            Ok(_) => match socket.listen(1024) {
                // `TcpSocket::listen` already returns a `tokio::net::TcpListener`,
                // so no `from_std` conversion is needed (and would be a type error).
                Ok(listener) => return Ok(listener),
                Err(e) => last_err = Some(e),
            },
            Err(e) => last_err = Some(e),
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "failed to bind HTTP listener after retries",
        )
    }))
}

/// 解析 `--http-endpoint`（`host:port/path`，IPv6 `[host]:port/path`）。
/// 可选 `http://` / `https://` 前缀被忽略（大小写不敏感），其余原样保留。
/// 缺端口默认 8082；缺 path 默认 /dynamic-mcp；path 经 normalize_path 归一化。
/// 任何非法输入返回 Err（人话报错，不 panic、不静默回落）。
fn parse_http_endpoint(input: &str) -> anyhow::Result<(String, u16, String)> {
    // 1. 忽略大小写敏感的 http(s):// 前缀
    let rest = if input.len() >= 7 && input[..7].eq_ignore_ascii_case("http://") {
        &input[7..]
    } else if input.len() >= 8 && input[..8].eq_ignore_ascii_case("https://") {
        &input[8..]
    } else {
        input
    };
    // 2. 拆 authority(host:port) 与 path
    let (authority, raw_path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    // 3. host + port
    let (host, port) = if authority.starts_with('[') {
        // IPv6：必须有闭合 ']'，否则明确报错（杜绝越界 panic）
        let end = authority
            .find(']')
            .ok_or_else(|| anyhow::anyhow!("IPv6 端点缺少闭合 ']'：{input}"))?;
        let h = &authority[1..end];
        // 缺 ':端口' → 默认 8082；有 ':端口' 但非法 → 报错
        let p = authority[end + 1..].strip_prefix(':').unwrap_or("8082");
        let port: u16 = p
            .parse()
            .map_err(|_| anyhow::anyhow!("端点端口非法（仅支持 0-65535）：{input}"))?;
        (h.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => {
                let port: u16 = p
                    .parse()
                    .map_err(|_| anyhow::anyhow!("端点端口非法（仅支持 0-65535）：{input}"))?;
                (h.to_string(), port)
            }
            None => (authority.to_string(), 8082), // 缺端口段 → 默认 8082
        }
    };
    if host.is_empty() {
        return Err(anyhow::anyhow!("端点必须包含 host：{input}"));
    }
    // 4. 归一化 path
    Ok((host, port, normalize_path(raw_path)))
}

/// 单例锁文件的规范 key，与 v1.8.0 的 format!("{}:{}{}", host, port, path) 逐字节一致。
fn canonical_endpoint(host: &str, port: u16, path: &str) -> String {
    format!("{}:{}{}", host, port, path)
}

/// 合并重复斜杠、保留单个前导 `/`、去除尾部斜杠；空结果回落 /dynamic-mcp。
fn normalize_path(raw: &str) -> String {
    let cleaned: String = raw
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if cleaned.is_empty() {
        "/dynamic-mcp".to_string()
    } else {
        format!("/{}", cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    #[test]
    #[serial]
    fn test_cli_arg_takes_precedence() {
        let cli_path = Some("cli-config.json".to_string());
        env::set_var("DYNAMIC_MCP_CONFIG", "env-config.json");

        let result = get_config_path(cli_path);
        assert!(result.is_some());

        let (path, source) = result.unwrap();
        assert_eq!(path, "cli-config.json");
        assert_eq!(source, "command line argument");

        env::remove_var("DYNAMIC_MCP_CONFIG");
    }

    #[test]
    #[serial]
    fn test_env_var_used_when_no_cli() {
        env::set_var("DYNAMIC_MCP_CONFIG", "env-config.json");

        let result = get_config_path(None);
        assert!(result.is_some());

        let (path, source) = result.unwrap();
        assert_eq!(path, "env-config.json");
        assert_eq!(source, "DYNAMIC_MCP_CONFIG environment variable");

        env::remove_var("DYNAMIC_MCP_CONFIG");
    }

    #[test]
    #[serial]
    fn test_no_config_returns_none() {
        env::remove_var("DYNAMIC_MCP_CONFIG");

        let result = get_config_path(None);
        assert!(result.is_none());
    }

    #[test]
    #[serial]
    fn test_empty_env_var_is_invalid() {
        env::set_var("DYNAMIC_MCP_CONFIG", "");

        let result = get_config_path(None);
        assert!(result.is_none());

        env::remove_var("DYNAMIC_MCP_CONFIG");
    }

    #[test]
    fn test_parse_http_endpoint_default() {
        let (h, p, path) = parse_http_endpoint("127.0.0.1:8082/dynamic-mcp").unwrap();
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 8082);
        assert_eq!(path, "/dynamic-mcp");
        assert_eq!(
            canonical_endpoint(&h, p, &path),
            "127.0.0.1:8082/dynamic-mcp"
        );
    }

    #[test]
    fn test_parse_http_endpoint_custom() {
        let (h, p, path) = parse_http_endpoint("0.0.0.0:9000/mcp").unwrap();
        assert_eq!(h, "0.0.0.0");
        assert_eq!(p, 9000);
        assert_eq!(path, "/mcp");
    }

    #[test]
    fn test_parse_http_endpoint_no_path() {
        let (_, _, path) = parse_http_endpoint("127.0.0.1:8082").unwrap();
        assert_eq!(path, "/dynamic-mcp");
    }

    #[test]
    fn test_parse_http_endpoint_trailing_slash() {
        let (_, _, path) = parse_http_endpoint("127.0.0.1:8082/dynamic-mcp/").unwrap();
        assert_eq!(path, "/dynamic-mcp");
    }

    #[test]
    fn test_parse_http_endpoint_double_slash() {
        let (_, _, path) = parse_http_endpoint("127.0.0.1:8082/a//b").unwrap();
        assert_eq!(path, "/a/b");
    }

    #[test]
    fn test_parse_http_endpoint_ipv6() {
        let (h, p, path) = parse_http_endpoint("[::1]:8082/path").unwrap();
        assert_eq!(h, "::1");
        assert_eq!(p, 8082);
        assert_eq!(path, "/path");
        assert_eq!(canonical_endpoint(&h, p, &path), "::1:8082/path");
    }

    #[test]
    fn test_parse_http_endpoint_ipv6_no_path() {
        let (_, _, path) = parse_http_endpoint("[::1]:8082").unwrap();
        assert_eq!(path, "/dynamic-mcp");
    }

    #[test]
    fn test_parse_http_endpoint_scheme_stripped() {
        let (h, p, path) = parse_http_endpoint("http://127.0.0.1:8082/mcp").unwrap();
        assert_eq!((h.as_str(), p, path.as_str()), ("127.0.0.1", 8082, "/mcp"));
    }

    #[test]
    fn test_parse_http_endpoint_port_zero() {
        let (h, p, path) = parse_http_endpoint("127.0.0.1:0/dynamic-mcp").unwrap();
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 0);
        assert_eq!(path, "/dynamic-mcp");
    }

    #[test]
    fn test_parse_http_endpoint_invalid_errors() {
        // 缺 ']' 的 IPv6
        assert!(parse_http_endpoint("[::1:8082/path").is_err());
        // 端口超界
        assert!(parse_http_endpoint("127.0.0.1:99999/x").is_err());
        // 非数字端口
        assert!(parse_http_endpoint("127.0.0.1:abc/x").is_err());
        // 无 host
        assert!(parse_http_endpoint("/dynamic-mcp").is_err());
        // 空串
        assert!(parse_http_endpoint("").is_err());
    }
}
