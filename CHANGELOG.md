# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.8.3] - 2026-07-25

### Fixed

- **B1（启动稳定性）** — 取消启动时对全部 group 连接的阻塞等待：各 group 连接改为在后台 `tokio::spawn` 任务中继续，`run_stdio` 立即进入 MCP 握手，不再 `await` 连接完成。避免 MCP 连接器在 dmcp 尚未就绪时因启动阶段握手/健康检查超时而将其杀掉重启，形成每 ~20–30 秒一次的慢速重启循环（冷启动 GNF 已在配置层 A2 + `mimo_mcp.py` 进程树清理处缓解）。
- **B3（响应正确性）** — `StdioTransport` 读取 `JsonRpcResponse` 时丢弃 `id` 与当前请求不匹配的陈旧响应，消除「请求 future 因超时丢弃、但上游随后回写其响应」导致的响应串扰（cross-talk）。`id` 为 null/缺失的通知与错误仍照常透传，保持原有行为。

> 本版本为连接器稳定性补丁，合并自 PR #8（`fix/mimo-stability-merge`），已剔除调试仪器化，对应版本号 bump 至 1.8.3。

## [1.8.2] - 2026-07-15

### Added

- **`--log` parameter (hybrid logging)** — New `--log <LEVEL>` CLI flag (`trace` / `debug` / `info` / `warn` / `error`; invalid values fall back to `warn`).
  - **Without `--log`**: `http` mode outputs WARN-level logs to stderr; `stdio` / `both` are silent; no log file. The `import` subcommand still initializes tracing as before.
  - **With `--log`**: all transport modes write a log file `dynamic-<pid>-<YYYYMMDD-HHMMSSmmm>.log` in the executable's directory (read-only fallback: `data_local_dir/dynamic-mcp/`). Additionally, `http` mode mirrors logs to stderr; `stdio` / `both` remain stderr-silent to protect JSON-RPC.
  - On startup, stale `dynamic-*.log` files older than 72 hours (excluding the current run) are cleaned up automatically.
  - No new dependencies added; uses `tracing-subscriber` with `env-filter` (already in `Cargo.toml`).

- **`get_dynamic_tools` enhanced parameters** — New optional parameters (defaults preserve byte-identical output with v1.8.1):
  - `mode` (`full` default / `compact`): `compact` returns tool name + full description, omits `inputSchema`.
  - `include_schema` (default `true`): set `false` to strip `inputSchema` from results.
  - `page` / `page_size` (defaults `1` / `0`): when `page_size > 0`, results are wrapped as `{tools, total, page, page_size, has_more}`; when `0`, flat array as before.
  - `land_to_file` (default `false`): writes results to a JSON file and returns the absolute path; files auto-cleaned after 72 hours.
  - `capabilities` (default `false`): attaches an `x-capabilities` array (from `GroupInfo.capability_tags`) to each tool entry.

- **`GroupInfo` metadata enrichment** — `list_groups` response now includes `tool_count` (number of tools in the group) and `capability_tags` (derived from transport type: `stdio` / `http` / `sse` / `oauth`). No `example` field (no source for it). `GroupInfo` has no `deny_unknown_fields`, so older clients remain compatible.

### Fixed

- **OAuth callback `localhost` → `127.0.0.1`** — The OAuth redirect URI used `localhost` while the callback listener bound to `127.0.0.1`; on some systems `localhost` resolves to `::1` (IPv6), causing the browser redirect to fail. Both sides now use `127.0.0.1` consistently.
- **OAuth discovery skipped when static `Authorization` header present** — Servers configured with a static `Authorization` header (i.e., `needs_oauth() == false`) no longer trigger an unnecessary OAuth discovery round-trip; they connect directly with the static header.
- **OAuth transport creation timeout 120s → 300s** — The timeout for creating the OAuth transport (browser authorization window) increased from 120s to 300s, giving users more time to complete browser-based authorization.

### Changed

- **Structured error envelope for `call_dynamic_tool`** — Tool call errors now return a structured JSON envelope (still via `CallToolResult` with `is_error: true`, not JSON-RPC error):
  ```json
  { "ok": false, "code": "<code>", "message": "<original message>", "cause": null }
  ```
  `code` mapping: `timed out` → `timeout` / `Tool execution failed` → `upstream_error` / `Missing required` → `bad_request` / other → `tool_error`. Existing tests asserting `is_error == Some(true)` remain unaffected.

## [1.8.1] - 2026-07-15

### ⚠️ Breaking Changes

- **Merged HTTP endpoint flags** — `--http-host` / `--http-port` / `--http-path` replaced by a single `--http-endpoint` (`host:port/path`, IPv6 `[host]:port/path`). Default `127.0.0.1:8082/dynamic-mcp`. Update startup scripts and LLM MCP configs.

### Fixed

- **IPv6 bind address** — binding to `::1` (or any IPv6 host) no longer fails to parse; the address is correctly bracketed before `SocketAddr` parsing (v1.8.0 crashed on IPv6 hosts).
- **Singleton conflict popup for IPv6** — the port-conflict popup now always shows the address (including IPv6) instead of being silently skipped.
- **macOS popup with newlines** — the `osascript` popup no longer fails silently when the message contains a newline (AppleScript string concatenation fix).

### Changed

- Singleton detection keys on the canonical `host:port/path` string (IPv6 brackets stripped), keeping cross-version lock-file compatibility with v1.8.0.
- Conflict / double-launch popup messages now reference `--http-endpoint` with a full example.

## [1.8.0] - 2026-07-14

### Added

- **Singleton / double-launch detection for HTTP endpoints** - When starting with `--transport http` or `both`, dynamic-mcp now detects whether another instance already owns the same HTTP endpoint (`--http-host` + `--http-port` + `--http-path`) and resolves the conflict automatically instead of failing silently or hitting a port clash.
  - A per-endpoint lock file under `~/.dynamic-mcp/locks/<sha256(endpoint)[..16]>.lock` records the owner's pid, transport, `--no-evict` flag, and executable path.
  - Decisions (pure, unit-tested `decide()`): a redundant `http` self-terminates after 8s; a `both` evicts an existing `http` (unless that http was started with `--no-evict`) and takes over the port 8s later with stdio available immediately; a `both` vs `both` (or vs a `--no-evict` http) keeps stdio only with HTTP off.
  - Stale locks are detected via pid liveness + executable-path comparison, so a reused pid cannot be mistaken for a live instance.
  - New `--no-evict` flag (valid only with `--transport http`): marks a plain http instance so a later `both` coexists (stdio only) instead of being evicted.
  - Two-layer user notification (double-launch `warn` + port-conflict `warn`) merged into a single popup: a real closable `MessageBoxW` on Windows, `notify-send` on Linux, `osascript` on macOS, plus a stderr line on every platform.
  - HTTP bind uses `SO_REUSEADDR` with a ~10s retry so an eviction can take over the port even while it is in `TIME_WAIT`.

## [1.6.0] - 2026-07-10

### Added

- **Streamable HTTP MCP transport (Route A)** - Expose the grouped-tool facade over a single Streamable HTTP endpoint, multiplexing multiple stdio upstream servers into three tools (`list_groups` / `get_dynamic_tools` / `call_dynamic_tool`).
  - New `--transport` CLI flag: `stdio` (default), `http`, or `both`
  - New `--http-host` (default `127.0.0.1`), `--http-port` (default `8082`), `--http-path` (default `/dynamic-mcp`) flags
  - `HttpFacadeHandler` implements `rmcp::ServerHandler` and serves the facade via `StreamableHttpService` mounted on an axum router (CORS-permissive)
- `list_groups` meta-tool now also exposed on the stdio surface (previously HTTP-facade only)

## [1.5.2] - 2026-07-09

### Fixed

- **list_groups Meta-Tool Implementation** - The `list_groups` tool was documented in v1.5.1 changelog but missing from the actual code
  - Added as the first built-in tool in `tools/list` response (before `get_dynamic_tools` and `call_dynamic_tool`)
  - Returns JSON array with group name, description, and status (`connected`/`failed`) for each MCP server
  - No input parameters required — works as a discovery tool independent of `enum` fields
  - Critical for interoperability with MCP proxies (e.g., mcp-bridge) that strip `enum` during JSON re-serialization

## [1.5.1] - 2026-07-09

### Added

- **list_groups Meta-Tool** - New built-in tool for MCP group discovery without enum dependency
  - LLMs can discover available MCP server groups when `enum` is stripped by proxy layers
  - Returns group name, description, and connection status (connected/failed) for each server
  - Resolves interoperability issue with MCP proxies (e.g., mcp-bridge using mcp-go) that lose `enum` fields during JSON re-serialization
  - No parameters required; works as a discovery tool before calling `get_dynamic_tools` or `call_dynamic_tool`

## [1.5.0] - 2026-02-14

### Added

- **Per-Server Timeout Configuration** - Custom timeouts per server for tools, resources, and prompts
  - Add `"timeout": { "tools": "30s", "resources": "30s", "prompts": "30s" }` to any server config
  - Supports formats: `30s`, `1min`, `1m`, `500ms`, plain numbers
  - Defaults: tools 30 seconds, resources 10 seconds, prompts 10 seconds
  - Useful for servers with long-running operations

## [1.4.0] - 2026-01-12

### Added

- **Per-Server Enable/Disable Control** - Optional `enabled` field to disable specific MCP servers
  - Add `"enabled": false` to any server config to skip connection
  - Defaults to `true` when omitted (all servers enabled by default)
  - Clean way to comment out servers without modifying config structure
- **Strict JSON Schema Validation** - Config files now enforce strict schema compliance
  - Only defined fields are allowed in config (denies unknown fields)
  - Catches typos and misspelled field names with clear error messages
  - Applies to all config levels: root, server objects, and feature flags
  - Helps prevent silent config errors and unexpected behavior
- **JSON Schema Reference Support** - Config files can now include `$schema` field for IDE validation

### Changed

- **Optional `type` Field for URL-Based Servers** - Simplified configuration for HTTP/SSE servers
  - `type` field is now optional when `url` is present (automatically defaults to `"http"`)
  - Automatic SSE detection per MCP spec when server responds with `text/event-stream`
  - Explicit `type: "sse"` still supported for SSE-only servers
  - Maintains full backwards compatibility with existing configs

## [1.3.0] - 2026-01-09

### Added

- **Per-Server Feature Flags** - Control which MCP APIs are exposed per server
  - Optional `features` field in server config (tools, resources, prompts)
  - Opt-out design: all features enabled by default for backward compatibility
  - Clear error messages when accessing disabled features
- **Interactive Feature Selection** - Import command now prompts for feature customization
  - Customize which features to enable during `dmcp import` workflow
  - Press Enter to keep all features (default) or type 'n' to customize
- **Resources API** - Full support for `resources/list`, `resources/read`, and `resources/templates/list`
  - Discover and retrieve file-like resources from upstream MCP servers
  - URI templates with RFC 6570 support for dynamic resource discovery
  - Resource size field for context window estimation
  - Cursor-based pagination and resource annotations
- **Prompts API** - Full support for `prompts/list` and `prompts/get`
  - Discover and retrieve prompt templates from upstream servers
  - Multi-modal prompt content (text, image, audio, embedded resources)
  - Argument substitution in prompt templates
  - Cursor-based pagination
- **SSE Stream Resumption** - Automatic recovery from interrupted SSE connections
  - Tracks Last-Event-ID to prevent event loss on reconnection
  - Handles both `id: value` and compact event ID formats

### Changed

- Resource and prompt operations now auto-discover server groups
- Config serialization omits default features for cleaner output
- SSE transport improved with event ID tracking

### Fixed

- Resource and prompt endpoints now work without explicit group parameter
- SSE connections properly resume from last known event after interruptions

## [1.2.0] - 2026-01-08

### Added

- **Multi-Tool Import Support** - Automatically import configs from 10 AI coding tools
  - Cursor, OpenCode, Claude Desktop, Claude Code CLI, VS Code
  - Cline, KiloCode, Codex CLI, Gemini CLI, Google Antigravity
- **Enhanced CLI** - `--global` flag for user-level configs, `--force` flag to skip prompts
- **Environment Variable Normalization** - Automatic conversion of tool-specific env var patterns
- **Config Parser Module** - Support for JSON, JSONC (with comments), and TOML formats
- **Tool Detection Module** - Smart path resolution for project/global configs per tool

### Changed

- Import command now uses tool names instead of file paths: `dmcp import cursor`
- Server processing order is now alphabetical (consistent interactive prompts)
- JSONC parsing improved with line comment stripping for better compatibility

### Documentation

- Updated README.md with tool-specific import examples and usage guides
- Updated IMPORT.md with comprehensive tool-specific import documentation

## [1.1.0] - 2026-01-07

### Added

- Python package distribution via PyPI with maturin bindings
- Windows ARM64 platform support in release binaries
- CHANGELOG.md included in GitHub release notes

### Changed

- Binary renamed from `dynamic-mcp` to `dmcp` for consistency with Python package
- Default logging level changed to `warn` (from `info`) for cleaner output
- Improved test reliability with better config fixtures and race condition handling

### Fixed

- Import command now respects `RUST_LOG` environment variable
- Removed duplicate wheel upload step in release workflow
- Updated dependencies: switched from native-tls to rustls for better ARM64 cross-compilation
- Snake_case tool names for better MCP protocol compliance
- Cross-platform process group handling for graceful shutdown

### Documentation

- Comprehensive AGENTS.md guide for AI-assisted development
- Expanded release process documentation
- Clearer installation instructions with uvx usage examples
- Updated README with restructured quick start and configuration sections

## [1.0.0] - 2026-01-06

### Added

- **Dynamic tool loading**: Expose only 2 proxy tools initially (`get_dynamic_tools`, `call_dynamic_tool`)
- **Multiple transport support**: stdio, HTTP, SSE for upstream MCP servers
- **OAuth2 authentication**: PKCE flow with automatic token refresh
- **Live configuration reload**: Watch config file changes and auto-reconnect
- **Automatic retry**: Exponential backoff for failed upstream connections
- **Import command**: Convert standard MCP configs to dynamic-mcp format (`dynamic-mcp import`)
- **Environment variable interpolation**: `${VAR}` syntax in configuration
- **Server descriptions**: Help LLMs understand when to use each tool group
- **Cross-platform binaries**: Linux x86_64, Linux ARM64, macOS ARM64, Windows x86_64

### Technical Details

- **Core**: Rust implementation using tokio async runtime
- **MCP Protocol**: rmcp v0.12 (official Rust MCP SDK)
- **HTTP Client**: reqwest with rustls-tls (pure Rust, no OpenSSL dependencies)
- **OAuth2**: oauth2 crate with PKCE support
- **File Watching**: notify crate for live reload
- **Testing**: 46 tests (37 unit + 9 integration), 100% pass rate
- **Lines of Code**: ~2,900 Rust

### Platform Support

- Linux x86_64 (`x86_64-unknown-linux-gnu`) - Native build
- Linux ARM64 (`aarch64-unknown-linux-gnu`) - Cross-compiled with rustls
- macOS ARM64 (`aarch64-apple-darwin`) - Native build for Apple Silicon
- Windows x86_64 (`x86_64-pc-windows-msvc`) - Native build

### Documentation

- Comprehensive README with quick start guide
- Architecture documentation explaining system design
- Import guide from standard MCP setup
- Security documentation for OAuth token storage
- Contributing guide with development setup
- Full API documentation via rustdoc

### Known Limitations

- Live reload works for config changes only (binary updates require restart)
- OAuth tokens stored as plain text in `~/.dynamic-mcp/oauth-servers/`
- No built-in rate limiting for tool calls
- Child processes inherit full privileges (no sandboxing)
- macOS Intel binaries are not released (build from source)
- Windows ARM64 binaries are not yet released (planned for future release)

### Installation

```bash
# From crates.io
cargo install dynamic-mcp

# Or download pre-built binaries from:
# https://github.com/asyrjasalo/dynamic-mcp/releases/tag/v1.0.0
```

### Links

- **crates.io**: https://crates.io/crates/dynamic-mcp
- **GitHub**: https://github.com/asyrjasalo/dynamic-mcp
- **Documentation**: https://docs.rs/dynamic-mcp
- **Release Notes**: [docs/implementation/mvp/RELEASE_v1.0.0.md](docs/implementation/mvp/RELEASE_v1.0.0.md)

[1.0.0]: https://github.com/asyrjasalo/dynamic-mcp/releases/tag/v1.0.0
[1.1.0]: https://github.com/asyrjasalo/dynamic-mcp/releases/tag/v1.1.0
[1.2.0]: https://github.com/asyrjasalo/dynamic-mcp/releases/tag/v1.2.0
[1.3.0]: https://github.com/asyrjasalo/dynamic-mcp/releases/tag/v1.3.0
[1.4.0]: https://github.com/asyrjasalo/dynamic-mcp/releases/tag/v1.4.0
