mod auth;
mod cli;
mod config;
mod http;
mod proxy;
mod server;
mod watcher;

use anyhow::Result;
use clap::{Parser, Subcommand};
use proxy::ModularMcpClient;
use server::ModularMcpServer;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;
use watcher::ConfigWatcher;

use http::server_handler::HttpFacadeHandler;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use std::net::SocketAddr;
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

    /// HTTP endpoint to mount the MCP Streamable HTTP server, as `host:port/path`.
    /// Default: `127.0.0.1:8082/dynamic-mcp`.
    #[arg(long, default_value = "127.0.0.1:8082/dynamic-mcp")]
    http_endpoint: String,

    /// Console log level for server mode (trace/debug/info/warn/error).
    /// Defaults: http/both -> warn, stdio -> error.
    /// RUST_LOG env var (if set) takes highest precedence.
    #[arg(long = "log-level", short = 'v', value_enum)]
    log_level: Option<LogLevel>,
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

/// Console log level for the server process, mapped 1:1 onto `tracing` levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum LogLevel {
    /// Most verbose: trace-level diagnostics
    Trace,
    /// Debug-level diagnostics
    Debug,
    /// Informational messages (e.g. the listening address)
    Info,
    /// Warnings only
    Warn,
    /// Errors only (least verbose)
    Error,
}

impl LogLevel {
    /// tracing directive string for this level (`error`/`warn`/`info`/`debug`/`trace`)
    fn as_str(self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

/// Default config file name searched next to the executable when no path is given.
const DEFAULT_CONFIG_FILENAME: &str = "dynamic-mcp.json";

/// Resolve the configuration file path from the available sources, in priority order:
/// 1. explicit CLI argument, 2. `DYNAMIC_MCP_CONFIG` env var, 3. `dynamic-mcp.json` beside the executable.
fn get_config_path(cli_arg: Option<String>) -> Option<(String, &'static str)> {
    // 1. Explicit CLI argument wins.
    if let Some(path) = cli_arg {
        return Some((path, "command line argument"));
    }

    // 2. DYNAMIC_MCP_CONFIG environment variable.
    if let Ok(path) = std::env::var("DYNAMIC_MCP_CONFIG") {
        if !path.is_empty() {
            return Some((path, "DYNAMIC_MCP_CONFIG environment variable"));
        }
    }

    // 3. Fallback: dynamic-mcp.json in the same directory as the running executable.
    if let Some(path) = default_config_next_to_exe() {
        return Some((path, "dynamic-mcp.json next to executable"));
    }

    None
}

/// Pure check: return `dynamic-mcp.json` under `dir` if it exists.
fn config_file_in_dir(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let candidate = dir.join(DEFAULT_CONFIG_FILENAME);
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}

/// If `dynamic-mcp.json` exists in the executable's directory, return its path as a string.
fn default_config_next_to_exe() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    config_file_in_dir(dir).and_then(|p| p.to_str().map(|s| s.to_string()))
}

/// Parse an HTTP endpoint of the form `host:port/path` (or `host:port`, or
/// `[::1]:port/path` for IPv6) into its components.
///
/// Returns `(host, port, path)`. The path defaults to `/dynamic-mcp` when
/// omitted. Errors on a missing/invalid port or malformed input.
fn parse_http_endpoint(endpoint: &str) -> Result<(String, u16, String), String> {
    // Split off the path (everything after the first '/').
    let (authority, path) = match endpoint.split_once('/') {
        Some((a, p)) if !p.is_empty() => (a, format!("/{}", p)),
        _ => (endpoint, "/dynamic-mcp".to_string()),
    };

    // Separate host and port. IPv6 addresses are wrapped in [..].
    let (host, port_str) = if let Some(rest) = authority.strip_prefix('[') {
        let close = rest
            .find(']')
            .ok_or_else(|| "missing ']' in IPv6 address".to_string())?;
        let host = &rest[..close];
        let after = &rest[close + 1..];
        let port_str = after
            .strip_prefix(':')
            .ok_or_else(|| "IPv6 address must be followed by ':port'".to_string())?;
        (host.to_string(), port_str.to_string())
    } else {
        let (host, port_str) = authority
            .rsplit_once(':')
            .ok_or_else(|| "missing ':' (expected host:port)".to_string())?;
        (host.to_string(), port_str.to_string())
    };

    if host.is_empty() {
        return Err("empty host".to_string());
    }
    let port: u16 = port_str
        .trim()
        .parse()
        .map_err(|_| format!("invalid port '{}' (expected 1-65535)", port_str))?;

    Ok((host, port, path))
}

/// Initialize the global `tracing` subscriber for server mode.
///
/// Gating rules (when neither `--log-level` nor `RUST_LOG` is set):
/// - `stdio`         -> `error` (keep stdout clean for the JSON-RPC protocol on stdout)
/// - `http`/`both`   -> `warn`  (so the "listening on ..." line is visible by default)
///
/// Precedence: `RUST_LOG` env var > `--log-level` CLI flag > transport default.
/// This fixes v1.6.0 where the server never initialized a subscriber, making
/// `RUST_LOG` a no-op and the console completely silent.
fn init_server_tracing(transport: TransportMode, log_level: Option<LogLevel>) {
    let default_directive = match transport {
        TransportMode::Stdio => "error",
        TransportMode::Http | TransportMode::Both => "warn",
    };
    let cli_directive = log_level.map(|l| l.as_str());

    // Precedence: RUST_LOG env var > --log-level CLI flag > transport default.
    let filter = match EnvFilter::try_from_default_env() {
        Ok(f) => f,
        Err(_) => {
            // RUST_LOG unset or invalid — fall back to CLI level, then transport default.
            let directive = cli_directive.unwrap_or(default_directive);
            EnvFilter::try_new(directive).unwrap_or_else(|_| EnvFilter::new(default_directive))
        }
    };

    // try_init returns Err (instead of panicking) if a global subscriber is
    // already installed — safe to call more than once.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .try_init();
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
            // Server mode: logging is initialized inside run_server() with a
            // transport-gated default level (stdio -> error keeps stdout clean for
            // the JSON-RPC protocol; http/both -> warn). RUST_LOG and --log-level
            // are both respected there.

            let (config_path, config_source) =
                get_config_path(cli.config_path).unwrap_or_else(|| {
                    eprintln!("Error: No configuration file specified");
                    eprintln!();
                    eprintln!("Usage: dynamic-mcp <config-file>");
                    eprintln!("   or: DYNAMIC_MCP_CONFIG=<config-file> dynamic-mcp");
                    eprintln!("   or: place dynamic-mcp.json next to the dynamic-mcp executable");
                    eprintln!();
                    eprintln!("Example: dynamic-mcp config.example.json");
                    eprintln!("     or: DYNAMIC_MCP_CONFIG=config.example.json dynamic-mcp");
                    std::process::exit(1);
                });

            run_server(
                config_path,
                config_source,
                cli.transport,
                cli.http_endpoint,
                cli.log_level,
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
    log_level: Option<LogLevel>,
) -> Result<()> {
    // Initialize logging first so the very first events below are captured.
    init_server_tracing(transport, log_level);

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

    // Initial load - spawn in background to avoid blocking stdio
    let client_init = client.clone();
    let config_path_init = config_path.clone();
    tokio::spawn(async move {
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

            for handle in handles {
                let _ = handle.await;
            }
        }
    });

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
    let http_enabled = matches!(transport, TransportMode::Http | TransportMode::Both);

    if http_enabled {
        let client_http = client.clone();
        let name_http = name.clone();
        let version_http = version.clone();
        let (host, port, path) = match parse_http_endpoint(&http_endpoint) {
            Ok(v) => v,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Invalid --http-endpoint '{}': {}",
                    http_endpoint,
                    e
                ));
            }
        };

        tokio::spawn(async move {
            let factory = move || {
                Ok::<_, std::io::Error>(HttpFacadeHandler::new(
                    client_http.clone(),
                    name_http.clone(),
                    version_http.clone(),
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

            match tokio::net::TcpListener::bind(addr).await {
                Ok(listener) => {
                    tracing::warn!(
                        "MCP Streamable HTTP server listening on http://{}{}",
                        addr,
                        path
                    );
                    if let Err(e) = axum::serve(listener, app).await {
                        tracing::error!("Streamable HTTP server error: {}", e);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to bind Streamable HTTP listener on {}: {}", addr, e);
                }
            }
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
    fn test_log_level_as_str() {
        assert_eq!(LogLevel::Trace.as_str(), "trace");
        assert_eq!(LogLevel::Debug.as_str(), "debug");
        assert_eq!(LogLevel::Info.as_str(), "info");
        assert_eq!(LogLevel::Warn.as_str(), "warn");
        assert_eq!(LogLevel::Error.as_str(), "error");
    }

    #[test]
    fn test_config_file_in_dir_finds_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(DEFAULT_CONFIG_FILENAME);
        std::fs::write(&cfg, "{}").unwrap();

        let found = config_file_in_dir(tmp.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap(), cfg);
    }

    #[test]
    fn test_config_file_in_dir_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(config_file_in_dir(tmp.path()).is_none());
    }

    #[test]
    fn test_parse_http_endpoint_default() {
        let (host, port, path) = parse_http_endpoint("127.0.0.1:8082/dynamic-mcp").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 8082);
        assert_eq!(path, "/dynamic-mcp");
    }

    #[test]
    fn test_parse_http_endpoint_no_path() {
        let (host, port, path) = parse_http_endpoint("0.0.0.0:9000").unwrap();
        assert_eq!(host, "0.0.0.0");
        assert_eq!(port, 9000);
        assert_eq!(path, "/dynamic-mcp");
    }

    #[test]
    fn test_parse_http_endpoint_ipv6() {
        let (host, port, path) = parse_http_endpoint("[::1]:8082/mcp").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 8082);
        assert_eq!(path, "/mcp");
    }

    #[test]
    fn test_parse_http_endpoint_bad_port() {
        assert!(parse_http_endpoint("127.0.0.1:notaport").is_err());
    }

    #[test]
    #[serial]
    fn test_exe_dir_fallback_used_when_no_cli_and_no_env() {
        // Simulate "no explicit config": no CLI arg and no DYNAMIC_MCP_CONFIG env var.
        env::remove_var("DYNAMIC_MCP_CONFIG");

        // The fallback only resolves if a dynamic-mcp.json sits next to the
        // executable. In the test environment there is none, so the result must
        // be None (rather than panicking or defaulting to a wrong path).
        let result = get_config_path(None);
        if let Some((path, source)) = result {
            assert!(
                path.ends_with(DEFAULT_CONFIG_FILENAME),
                "unexpected fallback path: {}",
                path
            );
            assert_eq!(source, "dynamic-mcp.json next to executable");
        }
    }
}
