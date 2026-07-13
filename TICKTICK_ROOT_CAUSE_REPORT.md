# dynamic-mcp 连接 TickTick（dida365）问题根因分析报告

> 分析对象：本地仓库 `D:\Documents\AI_Work_Temp\dynamic-mcp`（Rust，rmcp v0.12）
> 分析方式：纯代码静态分析（`src/config/schema.rs`、`src/proxy/transport.rs`、`src/proxy/client.rs`、`src/auth/oauth_client.rs`、`src/server.rs`、`examples/*`、`config-schema.json`）

---

## 0. 结论速览

| 问题 | 结论 |
|---|---|
| **A. 如何设置 `Streamable HTTP` 协议** | 本仓库**没有**名为 `streamable_http` 的类型。`type: "http"`（或省略类型）即对应 **Streamable HTTP** 实现；`type: "sse"` 才对应「旧版 SSE」传输。 |
| **B. 不设协议类型是否影响连接** | **不影响** Streamable HTTP 连接——缺少 `type` 且存在 `url` 时会被自动默认成 `http`。只有在「上游是旧版 SSE 服务器」时才需要显式写 `type: "sse"`，否则会连错协议。 |
| **C. 弹窗 + `Transport creation timed out` 的根因** | 配置文件**已经带了静态 `Authorization: Bearer ...`**，本不需要 OAuth；但代码在 `Transport::new` 中对 http/sse 传输**无条件调用了 `OAuthClient::authenticate()`**，触发浏览器授权弹窗；而又因为 header 已存在，`needs_oauth()` 判定为 `false`，外层超时只有 **5 秒**，OAuth 回调还没完成就超时了。回调“连不上 localhost”还有 **IPv4/IPv6 绑定不匹配**的二次成因。 |
| **2. `list_groups` 的 `description` 来源** | **直接取自配置文件的 `description` 字段**（完整字符串，未截断、未截取），而非来自上游 `initialize` 响应或任何派生。 |

---

## 1. 配置协议类型与传输层（`type` 字段）

### 1.1 仓库只支持三种传输类型

`src/config/schema.rs:184-234` 定义的枚举：

```rust
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum McpServerConfig {
    Stdio { description, command, args, env, features, enabled, timeout },
    Http  { description, url, headers, oauth_client_id, oauth_scopes, features, enabled, timeout },
    Sse   { description, url, headers, oauth_client_id, oauth_scopes, features, enabled, timeout },
}
```

配置 Schema 也只暴露 `enum: ["http", "sse"]`（`config-schema.json:119`，注释明确写 *“'http' for HTTP-only, 'sse' for SSE-only”*），**不存在 `streamable` / `streamable_http` 选项**。

### 1.2 `type: "http"` 就是 Streamable HTTP

`src/proxy/transport.rs:202-371` 的 `HttpTransport` 就是 Streamable HTTP 实现：

- 用 `reqwest` 向单个 `url` 发 `POST` JSON-RPC（`send_request`，行 252-281）；
- `Accept` 头同时声明 `application/json, text/event-stream`（行 263）；
- 既能解析普通 JSON 响应，也能按 `Content-Type: text/event-stream` 解析 SSE 响应（行 318-358）；
- 处理服务器在响应头下发的 `Mcp-Session-Id` 会话（行 286-295）。

这正是 **MCP Streamable HTTP 规范**（单一端点 + 可协商 SSE 响应）的行为。而 `SseTransport`（`transport.rs:373-553`）才是「旧版 SSE」：客户端需 `GET` 一个独立的 SSE 流端点、再 `POST` 到另一个端点。

### 1.3 不设 `type` 时的默认规则（关键）

`src/config/schema.rs:247-263` 的自定义反序列化逻辑：

```rust
if !obj.contains_key("type") {
    if obj.contains_key("url") {
        // 有 url 就默认 http
        obj.insert("type", "http");
    } else {
        obj.insert("type", "stdio");
    }
}
```

因此：
- **你这份配置没有 `type`，但有 `url` → 默认解析成 `McpServerConfig::Http` → 走 Streamable HTTP。** 这正是你想要的连接方式，所以**不写 `type` 并不影响 Streamable HTTP 连接**。
- 只有当上游是「旧版 SSE」（如 `.../sse` 端点、需要 GET 拉流）时，才必须写明 `"type": "sse"`，否则会被错误地按 http 去 POST 而失败。

**A 的正确写法（推荐显式声明，便于阅读与避免歧义）：**

```json
"TickTick": {
  "type": "http",
  "description": "TickTick滴答清单官方MCP服务。",
  "url": "https://mcp.dida365.com",
  "headers": {
    "Authorization": "Bearer dp_cbdf432a87574971a0ca243a81a79f18"
  },
  "timeout": { "tools": "3min", "resources": "2min", "prompts": "2min" }
}
```

> 补充提醒：Streamable HTTP 的端点通常是 `.../mcp` 这类带路径的地址。你给的是根路径 `https://mcp.dida365.com`，能弹出授权页说明 discovery 可达；但若 POST 根路径返回 404/405，还需确认 dida365 的真实 MCP 端点路径。这与下面的 C 是**两个独立问题**。

---

## 2. 问题 C 根因：弹窗 + `Transport creation timed out`

### 2.1 错误来自哪里

`src/proxy/client.rs:51-57`：

```rust
let transport = tokio::time::timeout(
    transport_timeout,                       // 注意这个时长
    Transport::new(&config_to_use, &group_name),
)
.await
.with_context(|| format!("Transport creation timed out for group: {}", group_name))?
```

你看到的 `failed / Transport creation timed out for group: TickTick` 就是这里抛出的——说明 `Transport::new` 在 `transport_timeout` 内没有返回。

### 2.2 超时为什么只有 5 秒

`src/proxy/client.rs:45-50`：

```rust
let needs_oauth = config.needs_oauth();
let transport_timeout = if needs_oauth {
    Duration::from_secs(120)   // OAuth 需要人工交互，给 120s
} else {
    Duration::from_secs(5)     // 否则只给 5s
};
```

`needs_oauth()` 的判断在 `src/config/schema.rs:420-446`：对于 http/sse，若 header 中**已经含有 `Authorization`**，则返回 `false`（因为 `!h.contains_key("Authorization")` 为 `false`）。

你的配置正好带了 `Authorization: Bearer ...`，所以 `needs_oauth() == false` → **外层超时只有 5 秒**。

### 2.3 真正的元凶：OAuth 被无条件触发

尽管 `needs_oauth()` 正确地认为“不需要 OAuth”，但这个结论**只被用来选超时时长**，并没有用来“跳过 OAuth”。

看 `src/proxy/transport.rs:570-612`（`Transport::new` 的 Http 分支）：

```rust
McpServerConfig::Http { url, headers, oauth_client_id, oauth_scopes, .. } => {
    let mut final_headers = headers.clone().unwrap_or_default();
    // ⚠️ 对 http/sse 一律调用 authenticate，不看 headers 里是否已有 Authorization
    let oauth_client = OAuthClient::new()?;
    match oauth_client.authenticate(server_name, url, oauth_client_id.as_deref(), oauth_scopes.clone()).await {
        Ok(token) => { /* 用新 token 覆盖 header */ }
        Err(e) => {
            if oauth_client_id.is_some() { return Err(e); }
            tracing::debug!("OAuth not available for {} (no client_id, discovery failed): {}", ...);
            // 无 client_id 时只是 debug 日志，继续用原 headers
        }
    }
    let transport = HttpTransport::new(url, Some(&final_headers)).await?;
    Ok(Transport::Http(transport))
}
```

`authenticate()` 在 `src/auth/oauth_client.rs:68-125` 里的逻辑是：

1. 先查缓存 token（首次运行没有）；
2. 没有有效缓存 → **立刻去 `discover_oauth_endpoints(url)`**（行 111），即请求 `https://mcp.dida365.com/.well-known/oauth-authorization-server`；
3. 只要 discovery 返回 200（dida365 **确实发布了 OAuth 元数据**），就进入 `perform_oauth_flow()`（行 113-121）。

而 `perform_oauth_flow`（`oauth_client.rs:189-262`）：

- `create_callback_server()` 随机绑定本地端口（行 197）；
- 没有 `oauth_client_id` → 先做动态客户端注册 `register_client()`（行 204）；
- **`open::that(auth_url)` 打开浏览器授权页（行 233）→ 这就是你看到的弹窗**；
- 接着 `wait_for_callback(listener)`（行 235）**阻塞等待浏览器回跳**，且该等待**没有任何内部超时**（见 2.5）。

### 2.4 完整的失败因果链

```
配置带静态 Authorization: Bearer ... （本不需要 OAuth）
   │
   ▼
Transport::new (transport.rs:581) 仍无条件调用 authenticate()
   │
   ▼
discover_oauth_endpoints 成功（dida365 有 /.well-known 文档）
   │
   ▼
perform_oauth_flow → open::that() 打开浏览器 → 弹窗出现
   │
   ▼
wait_for_callback 阻塞等待（无内部超时）
   │                                  同时：client.rs 外层 5s 超时在跑
   ▼
5 秒到 → tokio::time::timeout 触发 → Transport::new 被取消
   │
   ▼
"Transport creation timed out for group: TickTick"
   │
   ▼
被取消的 future 里 TcpListener 被 drop → 回调端口关闭
   │
   ▼
用户/浏览器后来才回跳 http://localhost:64710/oauth/callback
   → 端口已关闭 / 又叠加 IPv6 不匹配 → “浏览器连接不上”
```

**一句话根因**：代码把“是否走 OAuth”和“OAuth 是否超时”两件事割裂了——`needs_oauth()` 只决定超时长短，却没用来**跳过** OAuth；而只要上游公布了 OAuth discovery 文档，就会对“已自带 Bearer token”的配置发起一次阻塞式浏览器授权，于是 5 秒后必定超时失败。

### 2.5 二次成因：回调服务器只绑 IPv4，`localhost` 可能解析到 IPv6

`src/auth/oauth_client.rs:321-331`：

```rust
let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?; // 只绑 127.0.0.1
let port = listener.local_addr()?.port();
let redirect_url = RedirectUrl::new(format!("http://localhost:{}{}", port, CALLBACK_PATH))?;
```

- 监听端只绑 `127.0.0.1`（IPv4）；
- 但回跳地址用的是 `http://localhost:...`。在很多系统上 `localhost` 优先解析为 `::1`（IPv6），浏览器会去连 `::1:64710`，而服务端只听 `127.0.0.1` → **连接被拒绝**。

即便 5 秒超时没发生，这个 IPv4/IPv6 不匹配也会让回调到达不了，导致 OAuth 永远等不到 code。这是“浏览器连接不上 localhost:64710”的直接技术解释之一。

### 2.6 为什么 discovery 失败的服务器反而“正常”

注意 `transport.rs:597-608` 的 `Err` 分支：当 `authenticate()` 失败且**没有 `oauth_client_id`** 时，只是打一条 debug 日志并**继续用你写在配置里的 `headers`**。

换句话说：如果某个上游**没有** `/.well-known/oauth-authorization-server` 文档，`discover_oauth_endpoints` 会 early-bail → `authenticate` 返回 `Err` → dynamic-mcp 会**优雅回退到你的静态 Bearer token**，连接反而成功。dida365 之所以出问题，恰恰是因为它**太标准**（发布了 OAuth 元数据），触发了本不该发生的浏览器授权流程。

---

## 3. 问题 2：`list_groups` 的 `description` 从哪来

### 3.1 数据来源是配置文件的 `description`

`src/proxy/client.rs:37-42`（`connect` 入口）：

```rust
pub async fn connect(&mut self, group_name: String, config: McpServerConfig) -> Result<()> {
    ...
    let description = config.description().to_string();   // ← 取自配置
```

`config.description()` 在 `src/config/schema.rs:371-378` 直接返回枚举里的 `description` 字段（无论是 Stdio/Http/Sse 分支）。

连接成功后存入状态机 `GroupState::Connected { description, .. }`（`client.rs:169-178`），再被 `list_groups()` 原样读出（`client.rs:208-221`）：

```rust
pub fn list_groups(&self) -> Vec<GroupInfo> {
    self.groups.values().filter_map(|state| match state {
        GroupState::Connected { name, description, .. } => Some(GroupInfo {
            name: name.clone(),
            description: description.clone(),   // ← 原样返回，无任何截取/派生
        }),
        _ => None,
    }).collect()
}
```

`GroupInfo` 结构（`src/proxy/types.rs:3-7`）只有 `name` 和 `description` 两个字符串字段，下游（`server.rs`、`http/server_handler.rs`）再原样序列化返回给上层模型。

### 3.2 结论

- `list_groups` 返回的 `description` **就是配置文件 `description` 字段的完整值**；
- 它**不是**从上游 `initialize` 响应的 `serverInfo.description` 取来的（`connect` 里只用响应里的 `protocolVersion`，未使用其 description）；
- 它**不是“截取”**（没有截断、没有中间处理），是 1:1 透传；
- 另外注意：`list_groups` **只返回 `Connected` 状态的组**（`filter_map` 里 `_ => None`）。你这个 TickTick 因为 C 的问题处于 `Failed` 状态，所以**它根本不会出现在 `list_groups` 结果里**，而是出现在 `list_failed_groups()`（`client.rs:223-240`，其中 `description` 同样来自配置）的 `error` 字段里。

---

## 4. 修复建议

### 4.1 根治（代码层，推荐）

在 `src/proxy/transport.rs` 的 `Transport::new` 中，**当已存在 `Authorization` 等有效认证 header 时跳过 OAuth**：

```rust
McpServerConfig::Http { url, headers, oauth_client_id, oauth_scopes, .. } => {
    let mut final_headers = headers.clone().unwrap_or_default();
    // 已有静态鉴权头 → 不再发起 OAuth 浏览器流程
    let skip_oauth = oauth_client_id.is_none()
        && final_headers.contains_key("Authorization");
    if !skip_oauth {
        let oauth_client = OAuthClient::new()?;
        match oauth_client.authenticate(...).await {
            Ok(token) => { final_headers.insert("Authorization", format!("Bearer {}", token.access_token)); }
            Err(e) => { if oauth_client_id.is_some() { return Err(e); } /* else: 回退到静态 header */ }
        }
    }
    let transport = HttpTransport::new(url, Some(&final_headers)).await?;
    Ok(Transport::Http(transport))
}
```

这样你的“静态 Bearer token”配置就能直接连通，不会再弹窗、不会再 5 秒超时。

### 4.2 顺带修复 IPv4/IPv6 不匹配

`src/auth/oauth_client.rs:321-331` 的回调服务器改为同时接受 IPv6，或把回跳地址里的 `localhost` 换成 `127.0.0.1`：

```rust
let redirect_url = RedirectUrl::new(format!("http://127.0.0.1:{}{}", port, CALLBACK_PATH))?;
```

（bind 端也可改用 `SocketAddrV4`/`SocketAddrV6` 双栈监听，或 framep::…。）

### 4.3 临时规避（不改代码）

在当前代码下**没有干净的配置开关**能关掉 OAuth（因为 `authenticate` 无条件调用）。唯一“巧合式可用”的情形是：上游**不**发布 `/.well-known/oauth-authorization-server`，此时会回退到你的静态 header。dida365 恰好发布了该文档，所以无法靠改配置规避，必须改代码（4.1）。

---

## 5. 关键代码定位索引

| 现象 | 文件:行 | 说明 |
|---|---|---|
| 三种传输类型定义 | `src/config/schema.rs:184-234` | 仅 `stdio`/`http`/`sse`，无 `streamable` |
| 缺省 `type` → `http` | `src/config/schema.rs:247-263` | 有 `url` 默认 http |
| schema 仅允许 `http`/`sse` | `config-schema.json:119` | 印证无 streamable 类型 |
| `http` = Streamable HTTP 实现 | `src/proxy/transport.rs:202-371` | POST + 协商 SSE 响应 |
| 超时 5s / 120s 判定 | `src/proxy/client.rs:45-50` | 由 `needs_oauth()` 决定 |
| `needs_oauth()` 逻辑 | `src/config/schema.rs:420-446` | header 含 Authorization → false |
| 超时错误抛出点 | `src/proxy/client.rs:51-57` | “Transport creation timed out” |
| **无条件调用 OAuth** | `src/proxy/transport.rs:581-588` | 元凶 |
| OAuth discovery | `src/auth/oauth_client.rs:40-66,111` | 访问 `/.well-known/...` |
| 打开浏览器弹窗 | `src/auth/oauth_client.rs:233` | `open::that(auth_url)` |
| 回调阻塞无超时 | `src/auth/oauth_client.rs:333-347` | `wait_for_callback` |
| 回调仅绑 IPv4 | `src/auth/oauth_client.rs:321-331` | IPv6 不匹配二次成因 |
| `description` 取自配置 | `src/proxy/client.rs:42, 208-221` / `schema.rs:371-378` | Q2 答案 |
