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
use rmcp::transport::streamable_http_server::{StreamableHttpService, StreamableHttpServerConfig};
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

    /// HTTP host/address to bind when transport includes http
    #[arg(long, default_value = "127.0.0.1")]
    http_host: String,

    /// HTTP port to bind when transport includes http
    #[arg(long, default_value_t = 8082)]
    http_port: u16,

    /// HTTP path to mount the MCP Streamable HTTP endpoint
    #[arg(long, default_value = "/dynamic-mcp")]
    http_path: String,
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

            run_server(
                config_path,
                config_source,
                cli.transport,
                cli.http_host,
                cli.http_port,
                cli.http_path,
            )
            .await
        }
    }
}

async fn run_server(
    config_path: String,
    config_source: &str,
    transport: TransportMode,
    http_host: String,
    http_port: u16,
    http_path: String,
) -> Result<()> {
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
        let host = http_host;
        let port = http_port;
        let path = http_path;

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
                        "Failed to bind Streamable HTTP listener on {}: {}",
                        addr,
                        e
                    );
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
}
