# dynamic-mcp

> 一句话：它是个「工具中转站」——把你散落各处的 AI 工具收拢到一处，让 AI 助手用起来更省、更方便。

## 三分钟看懂：它到底是什么、怎么用（给不懂电脑的人）

先用人话讲清楚，技术细节放在后面。

### 一、它帮你解决两个麻烦

**麻烦 1：工具太多，AI 助手记不过来，还费钱**
AI 助手（比如 Claude、GPT）自己不会干活，得借用你准备好的各种「工具」——查资料、读文件、算数据……如果你有 20 个工具，全摆到助手面前，它得把每个工具说明都记在「工作桌面」上。桌面塞满，助手反应慢，而且每说一句话都要多花钱。

dynamic-mcp 的聪明做法：**一开始只给助手看一张写着 3 个选项的「菜单」**（列分组、看分组里有什么、调用某个工具）。助手真要用某个工具时，才临时把说明「调」出来。这样桌面始终干净，又省事又省钱。

**麻烦 2：助手够不着你电脑里的工具**
很多好用的工具只能「放在你电脑里、站你旁边才能用」。但助手如果在浏览器里、手机上、云端的另一台机器上，就「走不过去」拿。

dynamic-mcp 能把这些本地工具**接一根「电话线」，变成远处也能打的**（技术名词叫「把本地 stdio 桥接成 Streamable HTTP」）。接好后，远处的助手打一个「电话」（连上网络地址）就能用你电脑里的工具了。

### 二、两个概念先分清：启动「模式」 vs 实际「功能」

下面内容容易看晕，根因是把「模式」和「功能」混为一谈。先定死这两个词：

- **启动模式（开门方式）**：你用 `--transport` 这个命令行开关，选择**怎么把 dmcp 启动起来**。它有三个取值，就像「这间工具房开几扇门」：
  - `--transport stdio` → 只开「家门口那扇门」
  - `--transport http` → 只开「对外窗口」
  - `--transport both` → 两扇门都开
- **实际功能**：dmcp 在某个模式下**真正能提供的服务能力**，只有两种：
  - **stdio 功能**：通过标准输入/输出，和「你电脑旁桌面 AI 软件」对话（走家门口那扇门）。
  - **http 功能**：通过 HTTP 网络端点，让「浏览器 / 手机 / 云端的 AI 助手」远程调用（走对外窗口）。

**模式与功能的对应关系（全篇重点）**：

| 启动模式（`--transport`） | 开几扇门 | 实际功能 | 谁能连 |
|---|---|---|---|
| `stdio` | 只开家门 | 仅 **stdio 功能** | 本机桌面 AI 软件 |
| `http` | 只开窗口 | 仅 **http 功能** | 浏览器 / 手机 / 云端助手 |
| `both` | 两扇都开 | **stdio 功能 + http 功能** | 两类客户端同时用 |

> 📌 **一句话记住**：`stdio` / `http` / `both` 是「**怎么启动**」（开门方式），stdio 功能 / http 功能是「**启动后能干什么**」。模式决定开几扇门，功能就是门后面能用的能力。

> ⚠️ **一个例外（单例检测会自动降级）**：极少数端口冲突时，`both` 可能被自动降级为「只开 **stdio 功能**、http 功能关闭」（详见下方「HTTP 端点单例 / 双开检测」）。这是冲突自愈结果，非常态；正常情况下 `both` 两种功能都开。

### 三、模式三（both）的优点 / 亮点

模式三就是「一间房、两扇门、一套工具」，最大的好处是**一个程序顶两个用**：

1. **只开一个程序，两处都能用**：你电脑上的 AI 软件（WorkBuddy / Claude Desktop / Cursor / VS Code）走「家门口那扇门」（stdio），浏览器、手机、云端的 AI 助手走「对外窗口」（HTTP）——**一份程序同时服务两类助手**，不用开两个 dmcp。
2. **共享同一份工具连接和配置**：两种开门方式共用同一套上游工具和同一份配置文件，不需要维护两套 dmcp、连两次上游，省内存、省资源，配置只写一份，改一次两端同时生效。
3. **省心省力**：没有模式三的话，你要开两个程序——一个给桌面软件（stdio）、一个给浏览器（http），双倍连接、双倍内存、配置维护两处；有了模式三，这些都免了。

> ⚠️ **模式三该由谁来打开（很重要）**：应该**让你电脑上的 AI 软件（WorkBuddy / Claude Desktop / Cursor / VS Code）来帮你打开**，不要你自己手动点开。
> 原因：这间房「亮不亮灯」取决于「家门口那扇门有没有助手进来」。AI 软件把 dmcp 打开时，等于推开了家门口的门、站在房子里——房子通电，对外窗口也自然开了，浏览器里的助手就能从窗口进来。
> 如果你自己手动点开：家门口的门开了却没人用（浪费），你的 AI 软件还是用不了它，只能再去另开一个 → 又变两个程序，模式三的好处没了。
> 正确做法：在你的 AI 软件设置里写「用 both 方式打开 dmcp」，剩下的它自己会做。

> 💡 **小提醒**：房子亮灯靠「家门口那个助手在不在」。你关掉电脑上的 AI 软件，dmcp 会被关掉，对外窗口也关了——浏览器里的助手立刻用不了。如果你希望「哪怕电脑 AI 软件关了，浏览器助手照样能用」，就把模式三拆成两个独立程序：一个专门对外常亮的（模式二 http，你手动开着），一个给电脑 AI 软件自用的（模式一或模式三，由软件自己打开）。

### 四、一个参数怎么选（给想动手的人）

决定「开几扇门」的，是一个叫 `--transport` 的开关：
- 写 `stdio` → 只开家门口的门（给桌面 AI 软件）
- 写 `http` → 只开对外窗口（给浏览器/手机/云端）
- 写 `both` → 两扇门都开（让 AI 软件帮你打开）

其余几个参数（地址、端口、路径）一般不用改，保持默认即可；只有当你想让「本机以外」的设备也能访问时，才需要调整（详见下方技术章节的「参数」说明）。

---

下面是给开发者看的完整技术说明（看不懂不影响上面的大意）。

## 关于本分支（fork）使用 GitHub Actions 自行构建的说明

> **说明**：上游仓库（[asyrjasalo/dynamic-mcp](https://github.com/asyrjasalo/dynamic-mcp)）较长时间未更新。为了不等待上游发版即可使用本 fork 的新功能（含 HTTP 门面、端点单例检测等，截止 **v1.8.1**），本 fork 通过 **GitHub Actions** 自行构建二进制——具体由 Release 工作流在推送 `v*` 标签时触发。构建产物为跨平台二进制（含 Windows 的 `dmcp.exe`），作为 Release 资产（assets）发布于本 fork 的 Releases 页面。
>
> 这些构建**不会**发布到 crates.io / PyPI，**请勿使用 `cargo install` / `pip install` / `uvx` 安装 dynamic-mcp**；请直接从本 fork 的 Releases 页面下载二进制，或从源码编译（见下方「快速开始 → 安装」）。

## 快速开始

### 安装

#### 方式一：原生二进制

从本 fork 的 Releases 页面下载对应平台的可执行文件（无需 Rust 工具链、无需 Python）：

- **Linux x86_64**：`dmcp-x86_64-unknown-linux-gnu.tar.gz`
- **Linux ARM64**：`dmcp-aarch64-unknown-linux-gnu.tar.gz`
- **Windows x86_64**：`dmcp-x86_64-pc-windows-msvc.zip`（解压得 `dmcp.exe`）
- **Windows ARM64**：`dmcp-aarch64-pc-windows-msvc.zip`
- **macOS ARM64**：`dmcp-aarch64-apple-darwin.tar.gz`
- （暂无 macOS x86_64 构建；如需其他平台请走方式二源码编译）

下载解压后，将 `dmcp`（或 `dmcp.exe`）放到 `PATH` 中即可使用。

> 下载地址：https://github.com/zhangweildlh/dynamic-mcp/releases

#### 方式二：从源码编译

需要本机已安装 Rust 工具链（2021 edition，1.75+）：

```bash
git clone https://github.com/zhangweildlh/dynamic-mcp.git
cd dynamic-mcp
cargo build --release
# 产物：target/release/dmcp（Windows 为 target/release/dmcp.exe）
```

> 注：本仓库的二进制由 GitHub Actions 在推送 `v*` 标签时自动构建并发布到 Releases（见上方「关于本分支」说明），你通常无需自己编译。

### Dynamic 配置（dynamic-mcp.json）

#### 从上游 AI 编码工具导入

Dynamic-mcp 可以自动从上游 AI 编码工具导入 MCP 服务器配置。

**支持的工具**（`<tool-name>`）：

- Cursor（`cursor`）
- OpenCode（`opencode`）
- Claude Desktop（`claude-desktop`）
- Claude Code CLI（`claude`）
- Visual Studio Code（`vscode`）
- Cline（`cline`）
- KiloCode（`kilocode`）
- Codex CLI（`codex`）
- Gemini CLI（`gemini`）
- Google Antigravity（`antigravity`）

##### 快速开始

**从项目配置导入**（在项目目录中运行）：

```bash
dmcp import <tool-name>
```

**从全局 / 用户配置导入**：

```bash
dmcp import --global <tool-name>
```

**强制覆盖**（跳过确认提示）：

```bash
dmcp import <tool-name> --force
```

该命令会：

1. 检测你的工具配置位置
2. 解析已有的 MCP 服务器
3. 交互式提示输入描述
4. 交互式提示选择功能（tools、resources、prompts）
5. 规范化环境变量格式
6. 生成 `dynamic-mcp.json`

##### 导入示例

```bash
$ dmcp import cursor

🔄 Starting import from cursor to dynamic-mcp format
📖 Reading config from: .cursor/mcp.json

✅ Found 2 MCP server(s) to import

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Server: filesystem
Type: stdio

Config details:
  command: "npx"
  args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

💬 Enter description for 'filesystem' (what this server does): File operations on /tmp directory

🔧 Keep all features (tools, resources, prompts) for 'filesystem'? [Y/n]:
(press Enter to keep all features, or 'n' to customize)

[... prompts for other servers ...]

✅ Import complete!
📝 Output saved to: dynamic-mcp.json
```

**功能选择**：导入过程中，你可以自定义每个服务器启用哪些 MCP 功能：

- 按回车（或 Y）保留全部功能（tools、resources、prompts）
- 输入 `n` 来选择性启用 / 禁用单个功能
- 这样无需手动编辑配置文件即可实现细粒度控制

自定义功能选择示例：

```bash
🔧 Keep all features (tools, resources, prompts) for 'server'? [Y/n]: n

  Select features to enable (press Enter to accept default):
  Enable tools? [Y/n]: y
  Enable resources? [Y/n]: n
  Enable prompts? [Y/n]: n
```

##### 各工具注意事项

- **Cursor**：同时支持 `.cursor/mcp.json`（项目级）与 `~/.cursor/mcp.json`（全局级）
- **Claude Desktop**：仅全局配置，位置因系统而异：
  - macOS：`~/Library/Application Support/Claude/claude_desktop_config.json`
  - Windows：`%APPDATA%\Claude\claude_desktop_config.json`
  - Linux：`~/.config/Claude/claude_desktop_config.json`
- **Claude Code CLI**：同时支持 `.mcp.json`（项目根目录）与 `~/.claude.json`（用户 / 全局级）
- **Gemini CLI**：同时支持 `.gemini/settings.json`（项目级）与 `~/.gemini/settings.json`（全局级）
- **VS Code**：同时支持 `.vscode/mcp.json`（项目级）与用户级配置（各系统路径不同）
- **OpenCode**：同时支持 JSON 与 JSONC 格式（带注释的 JSON）
- **Codex CLI**：仅全局级 —— 使用 TOML 格式（`~/.codex/config.toml`）
- **Antigravity**：仅全局级 —— `~/.gemini/antigravity/mcp_config.json`

##### 环境变量转换

导入命令会自动把环境变量规范化为 dynamic-mcp 的 `${VAR}` 格式：

| 工具            | 原格式                 | 转换后             |
| --------------- | ---------------------- | ------------------ |
| Cursor          | `${env:GITHUB_TOKEN}`  | `${GITHUB_TOKEN}`  |
| Claude Desktop  | `${GITHUB_TOKEN}`      | `${GITHUB_TOKEN}`  |
| Claude Code CLI | `${GITHUB_TOKEN}`      | `${GITHUB_TOKEN}`  |
| VS Code         | `${env:GITHUB_TOKEN}`  | `${GITHUB_TOKEN}`  |
| Codex           | `"${GITHUB_TOKEN}"`    | `${GITHUB_TOKEN}`  |

**注意**：VS Code 的 `${input:ID}` 安全提示无法自动转换，导入后需手动配置。

详细的工具专属导入指南见 [docs/IMPORT.md](docs/IMPORT.md)。

#### 根据上游 MCP 服务器手动编写

> 💡 下方示例中的 `command: "npx"` / `command: "node"` 指的是**被代理的上游 MCP 服务器**（如 filesystem 服务器）自身的启动方式；`dmcp` 本身是你从「安装」一节下载 / 编译得到的二进制，**不要用 `npx` / `uvx` / `pip` 去装 dmcp**。

创建一个 `dynamic-mcp.json` 文件，为每个服务器填写 `description` 字段：

```json
{
  "mcpServers": {
    "filesystem": {
      "description": "Use when you need to read, write, or search files.",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    }
  }
}
```

##### 环境变量

支持使用 `${VAR}` 语法进行环境变量插值：

```json
{
  "mcpServers": {
    "example": {
      "description": "Example with env vars",
      "command": "node",
      "args": ["${HOME}/.local/bin/server.js"],
      "env": {
        "API_KEY": "${MY_API_KEY}"
      }
    }
  }
}
```

##### 服务器类型

支持所有[标准 MCP 传输机制](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)。

**注意**：当 `url` 存在时，`type` 字段是**可选**的。若省略，服务器会根据 MCP 规范自动使用 HTTP 传输并做 SSE 探测。这保持了与 [OpenCode](https://opencode.ai/docs/mcp-servers/) 等工具的向后兼容。

###### stdio（默认）

```json
{
  "description": "Server description for LLM",
  "command": "npx",
  "args": ["-y", "package-name"],
  "env": {
    "KEY": "value"
  }
}
```

###### http

```json
{
  "description": "HTTP server (type is optional)",
  "url": "https://api.example.com",
  "headers": {
    "Authorization": "Bearer ${TOKEN}"
  }
}
```

或显式指定类型：

```json
{
  "type": "http",
  "description": "HTTP server with explicit type",
  "url": "https://api.example.com",
  "headers": {
    "Authorization": "Bearer ${TOKEN}"
  }
}
```

###### sse

当服务器以 `Content-Type: text/event-stream` 响应时，SSE 服务器会被自动探测。若服务器仅支持 SSE，也可显式指定 `type: "sse"`：

```json
{
  "type": "sse",
  "description": "SSE server (explicit type required only if server doesn't auto-detect)",
  "url": "https://api.example.com/sse",
  "headers": {
    "Authorization": "Bearer ${TOKEN}"
  }
}
```

###### OAuth 认证（HTTP / SSE）

```json
{
  "description": "OAuth-protected MCP server (type is optional)",
  "url": "https://api.example.com/mcp",
  "oauth_client_id": "your-client-id",
  "oauth_scopes": ["read", "write"]
}
```

**OAuth 流程：**

- 首次连接时，浏览器会打开以进行授权
- 访问令牌保存在 `~/.dynamic-mcp/oauth-servers/<server-name>.json`
- 在过期前自动刷新令牌（支持 RFC 6749 令牌轮换）
- 令牌以 `Authorization: Bearer <token>` 请求头形式注入

##### 功能开关（Feature Flags）

使用可选的 `features` 字段，按服务器控制暴露哪些 MCP 功能。默认情况下全部功能（`tools`、`resources`、`prompts`）均启用。你可以选择性地禁用某些功能：

```json
{
  "mcpServers": {
    "server-with-tools-only": {
      "description": "Server that only exposes tools",
      "command": "npx",
      "args": ["-y", "some-mcp-server"],
      "features": {
        "resources": false,
        "prompts": false
      }
    },
    "server-without-prompts": {
      "description": "HTTP server without prompt templates (type is optional)",
      "url": "https://api.example.com",
      "features": {
        "prompts": false
      }
    }
  }
}
```

**行为：**

- 若省略 `features`，则全部功能启用（opt-out 设计）
- 若指定了 `features`，未提及的功能默认仍为 `true`（启用）
- 被禁用的功能通过代理访问时会返回错误
- 例如：若 `resources: false`，调用 `resources/list` 会返回错误

##### 禁用服务器

使用可选的 `enabled` 字段，在不从配置中删除服务器的前提下将其禁用：

```json
{
  "mcpServers": {
    "filesystem": {
      "description": "File operations",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    },
    "disabled-server": {
      "description": "This server won't connect",
      "command": "some-command",
      "enabled": false
    }
  }
}
```

**行为：**

- 若省略 `enabled`，服务器为启用状态（默认行为）
- 若 `enabled: false`，则在连接阶段跳过该服务器，不会出现在可用分组中
- 适用于测试或维护期间临时禁用服务器而无需改动配置结构
- 完整示例见 `examples/config.features.example.json`

##### 超时配置

使用可选的 `timeout` 字段，按服务器自定义工具、资源、提示调用的超时时间。默认值：

- 工具调用：30 秒
- 资源调用：10 秒
- 提示调用：10 秒

你可以为需要更长时间的服务器自定义：

```json
{
  "mcpServers": {
    "slow-server": {
      "description": "Server with slow operations",
      "command": "npx",
      "args": ["-y", "some-slow-mcp-server"],
      "timeout": {
        "tools": "1min",
        "resources": "30s",
        "prompts": "30s"
      }
    }
  }
}
```

**支持的时长格式：**

| 格式         | 示例             | 说明                 |
| ------------ | ---------------- | -------------------- |
| 秒           | `"30s"`、`"5s"`  | 简单秒数             |
| 分钟         | `"1min"`、`"2m"` | 分钟（缩写或完整）   |
| 毫秒         | `"3000ms"`、`"500ms"` | 毫秒           |
| 纯数字       | `30`             | 秒（纯数字）         |

**行为：**

- 若省略 `timeout`，使用默认值（tools: 30s，resources: 10s，prompts: 10s）
- 单个超时字段若未指定，默认取各自对应的默认值
- 仅适用于工具 / 资源 / 提示调用操作，不适用于连接或初始化
- 适用于存在长时间运行操作的服务器（数据库查询、文件处理等）

### Dynamic 启动（含命令行启动）

> 📌 下面对应上方「两个概念」的映射：`stdio` 模式 → 仅 stdio 功能；`http` 模式 → 仅 http 功能；`both` 模式 → 两种功能都有。三种模式「由谁启动」不同，是易错点，请重点看。

#### 模式一 · stdio（默认，`--transport stdio`）

- **是什么**：dynamic-mcp 作为你本机的一个子进程运行，数据通过标准输入/输出（stdio）收发。
- **由谁启动**：stdio 模式的**正常用法是由 LLM 客户端拉起**——在 WorkBuddy / Claude Desktop / Cursor / VS Code 等桌面客户端的 MCP 配置里写 `"command": "dmcp"`，由客户端自己把 dmcp 启动起来、并接管它的标准输入/输出；进程随客户端启动而生、随客户端退出而灭。**你当然也可以在终端里手动运行 `dmcp config.json`**，但那样它的标准输入/输出只连着你的终端、没有 LLM 客户端接管，等于没有「对话对象」，无法正常使用——所以 stdio 模式只有被 LLM 客户端拉起才有实际意义。
- **适用场景**：仅在你本机、使用桌面 AI 客户端时。
- **限制**：浏览器、云端、手机上的 LLM 没有能力在你电脑上拉起一个本地进程，因此**这些环境用不了 stdio 模式**。

#### 模式二 · HTTP（`--transport http`）

- **是什么**：dynamic-mcp 作为**常驻的 HTTP 服务**运行，对外暴露一个 Streamable HTTP MCP 端点（`http://<host>:<port><path>`），任何能发 HTTP 请求的客户端都能连。
- **由谁启动**：**只能由你（用户）手动启动，LLM 不能拉起它**。原因——模式二正是为了弥补 stdio 的短板而新增：浏览器、云端、移动端里的 LLM 应用，根本无法在你的机器上 spawn 一个本地子进程；所以必须**先由你自己在终端或服务里把它跑起来、让它一直监听端口**，远端的 LLM 才能连上来。**在模式二中，LLM 只是「连接者」，永远不是「启动者」**。
- **适用场景**：浏览器插件、云端 Agent、手机 App、远程 / 容器（Docker、k8s）环境，或需要被多个客户端共享同一个后端时。
- **最小启动命令**：

  ```bash
  dmcp --transport http /path/to/dynamic-mcp.json
  ```

#### 模式三 · both（`--transport both`）

- **是什么**：stdio 与 HTTP 同时开启——一个进程，两套入口（对外窗口 + 家门口的门）。
- **亮点 / 优点**：
  - **一个程序顶两个用**：桌面 AI 软件走 stdio 门、浏览器/手机/云端助手走 HTTP 窗口，**一份进程同时服务两类客户端**，不用开两个 dmcp。
  - **共享同一份上游连接和配置**：两种入口共用同一套上游工具与同一份配置文件，省内存、省资源，配置只写一份。
  - **省心**：没有 `both` 就得开两个程序（一个 stdio 给桌面、一个 http 给浏览器），双倍连接、双倍内存、配置维护两处；`both` 免了这些。
- **由谁启动（关键）**：**应由你的桌面 AI 软件（WorkBuddy / Claude Desktop / Cursor / VS Code）拉起**，不要自己手动在终端启动。软件拉起 dmcp 时推开 stdio 门、站在房子里，进程随之常驻，HTTP 窗口也自然开着，浏览器助手即可连入；若手动启动，stdio 门空转、桌面软件用不上，等于白开 `both`。通俗讲解见上方「三分钟看懂」。
- **适用场景**：本机桌面端「省 token」与浏览器/云端/手机远程使用同一套上游工具，二者同时需要。

### 参数（命令行）

新增的命令行参数用于控制 HTTP 暴露方式（**配置文件无需改动**，见下文）：

| 参数            | 默认值          | 说明（人话）                                      |
| --------------- | --------------- | ------------------------------------------------- |
| `--transport`   | `stdio`         | 决定开几扇门：`stdio` 只给桌面 AI 软件用；`http` 只给浏览器/手机/云端用；`both` 两者都要（推荐让桌面 AI 软件帮你打开）。 |
| `--http-endpoint` | `127.0.0.1:8082/dynamic-mcp` | 对外窗口的「完整地址」：`host:port/path`（IPv6 用 `[host]:port/path`）。客户端连接地址要和这里完全一致，例如 `http://127.0.0.1:8082/dynamic-mcp`。默认即可，端口被占用就改（如 `127.0.0.1:9000/dynamic-mcp`）。 |
| `--no-evict`    | `false`         | 仅对 `--transport http` 有效。给当前的纯 http 实例"上锁"：告诉将来同端口启动的 `both` 实例"别强杀我"，让 `both` 只开 stdio、HTTP 关掉，二者和平共存。配 `--transport both` 或 `stdio` 时会直接报错退出。 |

### 使用方法

```bash
# 仅 stdio（无 HTTP 功能）：
dmcp /path/to/dynamic-mcp.json
dmcp --transport stdio /path/to/dynamic-mcp.json

# 仅 HTTP（无 stdio 功能；用于不能自主启动 `dynamic-mcp` 的 LLM，如 网页版 LLM / 移动端 LLM 等）：
dmcp --transport http /path/to/dynamic-mcp.json

# both（用于能自主启动 `dynamic-mcp` 的 LLM，如 WorkBuddy / OpenCode / Claude Desktop 等）：
dmcp --transport both /path/to/dynamic-mcp.json

# 指定对外端点（host:port/path；IPv6 用 [host]:port/path）：
dmcp --transport http --http-endpoint 0.0.0.0:9000/mcp /path/to/dynamic-mcp.json
```

当使用 `--transport http` 或 `both` 时，门面服务地址为 `http://<host>:<port><path>`（例如 `http://127.0.0.1:8082/dynamic-mcp`）。

> 💡 **网页 LLM 里地址到底怎么填（重点）**：
> - 完整端点 = `http://<host>:<port><path>`，其中 `<path>` 就是你 `--http-endpoint` 里 `host:port` 之后的那段 path（如 `/dynamic-mcp`），**前后都不要再加减 `/mcp`**。
> - 例如你用 `--http-endpoint 127.0.0.1:8082/dynamic-mcp-server`，客户端就填 `http://127.0.0.1:8082/dynamic-mcp-server`（已实测可连）。
> - ⚠️ 若填成 `http://127.0.0.1:8082/mcp/dynamic-mcp-server` 或 `.../dynamic-mcp-server/mcp` 都会报 **HTTP 404**——本服务不自带 `/mcp` 前缀，多加一段就是路径错。
> - 启动后控制台若毫无输出也别慌：v1.8.1 服务器模式默认静默（见下方「故障排查 → 日志」），只要浏览器不是报「无法连接」，就说明它正在监听。

> 💡 **WorkBuddy / OpenCode / Claude Desktop 里地址怎么填**：
> 这些桌面客户端通常会以两种方式和 dmcp 协作：
> - **由客户端拉起（stdio / both）**：在客户端的 MCP 配置里写 `"command": "dmcp"`、`"args": ["--transport", "both", "/abs/path/dynamic-mcp.json"]`，客户端自己负责启动与 stdio 通信，你不需要手动填 HTTP 地址。
> - **以「HTTP MCP 服务器」方式连接 dmcp 的对外窗口**：当客户端需要直接填一个 HTTP 端点时，就填 `http://<host>:<port><path>`：
>   - dmcp 用默认 `--http-endpoint 127.0.0.1:8082/dynamic-mcp` 时，填 `http://127.0.0.1:8082/dynamic-mcp`。
>   - 客户端与 dmcp 同机时 `host` 用 `127.0.0.1` 即可；若 dmcp 跑在另一台机器，填那台的局域网 IP（且 dmcp 需用 `0.0.0.0` 或该 IP 作 host）。
>   - 同样**不要额外加 `/mcp` 前缀**，否则 404。

## 与上游仓库（v 1.5.0）相比较，本 Fork 仓库（截止v 1.8.1）新增的功能

> 上游仓库（[asyrjasalo/dynamic-mcp](https://github.com/asyrjasalo/dynamic-mcp)）停留在 v1.5.0 附近，本 fork 在此基础上持续迭代。以下按**功能**汇总本 fork（v1.6.0 → v1.8.1）相比上游新增的能力；同一能力在多版本演进的，以最终形态为准（后续版本覆盖前序版本，不重复列举版本号）。

### 1. Streamable HTTP 门面与多模式传输

本 fork 让 dynamic-mcp 不仅能做「stdio 代理」，还能把分组后的工具**通过单一 Streamable HTTP 端点对外暴露**，从而让浏览器、云端、手机上的 LLM 也能调用你本机的工具。

- 新增 `--transport` 开关，三种模式：`stdio`（默认，只给桌面客户端）、`http`（只开对外窗口）、`both`（stdio + HTTP 同时开）。
- HTTP 门面把多个 stdio 上游服务器复用为 3 个工具对外暴露：`list_groups`（列分组）、`get_dynamic_tools`（按需拉某分组的工具说明）、`call_dynamic_tool`（调用具体工具）。客户端不再需要一次性加载全部工具 schema，"省桌面、省 token" 的设计扩展到远程场景。
- `list_groups` 元工具同时也暴露在 stdio 表面（此前仅 HTTP 门面有），便于不支持 `enum` 的代理层做服务发现。

**应用场景**：你用 Claude Desktop（stdio）在本机干活，同时希望浏览器里的网页 LLM 也能调用同一批工具。运行 `dmcp --transport both config.json`：桌面端走 stdio 门、浏览器端连 `http://127.0.0.1:8082/dynamic-mcp`，一份进程同时服务两类客户端，配置只写一份。

### 2. HTTP 端点单例 / 双开检测与 `--no-evict`

启动 `--transport http` / `both` 时，本 fork 会自动检测**同一个 HTTP 端点是否已有另一个实例在跑**，并自动化解冲突，不再静默失败或端口撞车。

- 每个端点一把锁文件：`~/.dynamic-mcp/locks/<sha256(endpoint) 前 16 位>.lock`，记录 owner 的 pid、传输模式、`--no-evict` 标志与可执行路径。
- 通过 **pid 存活 + 可执行路径比对**识别过期锁，避免「pid 被复用」误判为存活实例。
- 冲突决策（纯函数 `decide()`，已单测）：
  - 多余的 `http` 实例：8 秒后**自我终止**，把端口让给先来的。
  - 后到的 `both`：会**接管（evict）**已有的 `http`（除非那个 http 启动时带了 `--no-evict`），约 8 秒后占用端口、stdio 立即可用。
  - `both` vs `both`（或 vs 带 `--no-evict` 的 `http`）：保留先来的，**后者只开 stdio、HTTP 关掉**，二者共存不冲突。
- 新增 `--no-evict` 参数（仅对纯 `http` 有效）：标记「这个 http 实例很重要，别杀我」，让后来的 `both` 与之和平共存（只开 stdio）。
- 双层通知合并为单个弹窗：Windows `MessageBoxW` / Linux `notify-send` / macOS `osascript`，外加每平台 stderr 一行提示。
- HTTP 绑定使用 `SO_REUSEADDR` + 约 10 秒重试，使接管能在端口处于 `TIME_WAIT` 期间完成。

**应用场景**：
1. 你手滑点了两次 `dmcp --transport both`——第二次检测到第一次已占端口，自动只开 stdio、HTTP 关掉，避免「端口被占用」崩溃。
2. 你先开一个常驻 `dmcp --transport http --no-evict config.json`（专门给浏览器用），后来桌面软件又拉起一个 `both`——因为 http 带了 `--no-evict`，`both` 不杀它，自己只开 stdio，两个实例和平共存。

### 3. `--http-endpoint` 单一参数（破坏性变更）+ IPv6 / 弹窗修复

本 fork 把原先分立的 `--http-host` / `--http-port` / `--http-path` **合并为单一 `--http-endpoint`**（`host:port/path`，IPv6 用 `[host]:port/path`），默认 `127.0.0.1:8082/dynamic-mcp` 不变。旧三参数已移除，启动脚本与 LLM 的 MCP 配置需同步改为单参。

随本次合并修复了 3 个缺陷：
- **IPv6 端点绑定崩溃**：绑定地址在解析前自动补方括号（v1.8.0 在 IPv6 host 上直接崩溃）。
- **IPv6 单例弹窗 canonical key 错误**：单例锁的规范化 key 改为 `host:port/path`（IPv6 去方括号），弹窗始终显示地址（含 IPv6）。
- **macOS 弹窗多行换行被吞**：`osascript` 改用 `" & return & "` 拼接多行，避免换行丢失导致弹窗静默失败。

**应用场景**：
- 想让局域网其他设备也能连：`dmcp --transport http --http-endpoint 0.0.0.0:9000/mcp config.json`。
- 本机走 IPv6：`dmcp --transport http --http-endpoint "[::1]:8082/dynamic-mcp" config.json`（v1.8.1 起不再崩溃）。

### 4. 默认配置文件名 `dynamic-mcp.json`

本 fork 将配置文件默认名从 `dmcp_config.json` 改为 `dynamic-mcp.json`，与二进制名 `dmcp` 统一。查找优先级：**CLI 位置参数** → **`DYNAMIC_MCP_CONFIG` 环境变量** → **可执行文件同目录的 `dynamic-mcp.json`**。

**应用场景**：把配置命名为 `dynamic-mcp.json` 放在 `dmcp` 二进制同目录，直接运行 `dmcp` 即可加载，无需每次传路径参数。

### 5. Fork 通过 GitHub Actions 自行构建发布二进制

由于上游长期未发版，本 fork 改为**自行构建**：推送 `v*` 标签时，GitHub Actions 的 Release 工作流自动编译跨平台二进制（Linux x86_64 / ARM64、Windows x86_64 / ARM64、macOS ARM64）并发布到本 fork 的 Releases；**不发布到 crates.io / PyPI**。用户无需本地 Rust 工具链即可获取可用二进制（见上方「快速开始 → 安装」）。

**应用场景**：你不装 Rust 工具链，也能从 [Releases](https://github.com/zhangweildlh/dynamic-mcp/releases) 下载 `dmcp` 直接用；而上游仓库没有对应的最新二进制可用。

## 故障排查

### 服务器连接问题

**问题**：`❌ Failed to connect to <server>`

**解决方案**：

- **连接超时**：每个服务器在传输创建、初始化与工具列举上各有 10 秒超时
- **自动重试**：失败服务器最多重试 3 次，采用指数退避（2s、4s、8s）
- **周期性重试**：失败服务器在后台每 30 秒重试一次
- **慢速 HTTP 服务器**：远程 HTTP / SSE 服务器若响应慢会超时并被自动重试
- **stdio 服务器**：确认命令存在（`which <command>`）
- **HTTP / SSE 服务器**：检查服务器是否在运行、URL 是否正确
- **环境变量**：确保全部 `${VAR}` 引用均已定义
- **OAuth 服务器**：按提示完成 OAuth 流程

### 日志与「控制台没有输出」（重要）

在 **v1.8.1** 中，运行服务器（`--transport stdio` / `http` / `both`）时，**控制台默认是空的、没有任何日志输出**——这不是程序崩溃，而是当前版本的设计行为：

- 只有 `import` 子命令会初始化日志系统；**运行服务器时根本没有初始化 tracing subscriber**，因此无论你是否设置 `RUST_LOG` 环境变量，服务器模式都不会打印任何日志（连 INFO 级的「已在 xxx 监听」提示都会被抑制）。
- **当前构建没有 `--log-level` / `-v` 参数，无法用命令行开关日志级别。** 该参数曾在 v1.7.0 提供（当时与 `RUST_LOG` 配合：http/both 默认 `warn`、stdio 强制静默以保护 JSON-RPC），但在 **v1.8.0** 的重构中被移除。
- **为何移除**：stdio 模式必须保证 stderr/stdout 干净，否则会污染 JSON-RPC 协议、导致客户端解析失败。v1.7.0 用「按传输模式分别开关日志」的复杂逻辑来兼顾，v1.8.0 简化为「服务器模式一律静默」——既彻底规避污染风险，也省去维护开关的成本。代价是：**当前版本无法通过命令行或 `RUST_LOG` 调高 http/both 模式的日志详细度**（http 用户排查问题时，只能从客户端侧或进程存活状态判断，详见下方「怎样判断它在运行」）。
- 怎样判断它真的在运行？用浏览器 / 网页 LLM 连接时，如果返回的是 **`HTTP 404`**（而不是「无法连接 / connection refused」），就说明服务器已经在监听了，只是你填的**路径不对**（见上方「参数 → --http-endpoint」的 404 提醒）。

> ⚠️ **别被空控制台骗了**：看到终端里一行都没有，以为没启动成功——其实它正安静地监听在 `127.0.0.1:8082`。「监听成功」那行日志只是被屏蔽了而已。

### OAuth 认证问题

**问题**：浏览器未打开进行 OAuth

**解决方案**：

- 手动打开控制台显示的 URL
- 检查防火墙是否允许 localhost 连接
- 确认服务器的 `oauth_client_id` 正确

**问题**：令牌刷新失败

**解决方案**：

- 删除缓存令牌：`rm ~/.dynamic-mcp/oauth-servers/<server-name>.json`
- 下次连接时重新认证

### 环境变量未被替换

**问题**：配置中显示的是 `${VAR}` 而非实际值

**解决方案**：

- 使用 `${VAR}` 语法，而非 `$VAR`
- 导出变量：`export VAR=value`
- 变量名区分大小写
- 检查变量名是否拼写错误

### 配置错误

**问题**：`Server missing 'description' field`

**解决方案**：

- 配置中的每个 MCP 服务器都必须有 `description` 字段
- 该描述用于向 LLM 解释服务器用途
- 示例：

  ```json
  {
    "description": "File system access - read, write, and search files",
    "command": "npx",
    "args": ["@modelcontextprotocol/server-filesystem"]
  }
  ```

**问题**：`Invalid JSON in config file`

**解决方案**：

- 校验 JSON 语法（使用 `jq . config.json`）
- 检查是否有尾随逗号
- 确保所有必需字段齐全（`description` 始终必需；`type` 仅 http/sse 服务器必需）

**问题**：配置中存在未知字段（如 `unknown field \`typo_field\`\`）

**解决方案**：

- dynamic-mcp 使用严格的 JSON Schema 校验，仅允许已定义的字段
- 检查字段名拼写：`description`、`command`、`url`、`type`、`args`、`env`、`headers`、`oauth_client_id`、`oauth_scopes`、`features`、`enabled`、`timeout`
- 从配置中移除任何多余或拼写错误的字段
- 参考上文各服务器类型的示例查看合法字段

**问题**：`Failed to resolve config path`

**解决方案**：

- 使用绝对路径或相对于工作目录的路径
- 检查文件是否存在且具有读权限
- 尝试：`ls -la <config-path>`

### 工具调用失败

**问题**：工具调用返回错误

**排查**：

1. 直接使用上游服务器测试该工具
2. 检查工具名与参数是否与 Schema 匹配
3. 确认分组名正确
4. 开启 debug 日志查看 JSON-RPC 消息

### 性能问题

**问题**：启动缓慢

**解决方案**：

- 已启用并行连接
- 检查 HTTP / SSE 服务器的网络延迟
- 部分服务器初始化较慢属正常现象

**问题**：内存占用高

**解决方案**：

- 工具会缓存在内存中（预期行为）
- 失败的分组占用内存极少
- 大型工具 Schema 会增加内存占用

## 贡献

关于开发环境搭建、测试与贡献的说明，见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 版本历史

版本历史与发版说明见 [CHANGELOG.md](CHANGELOG.md)。

## 致谢

- TypeScript 实现：[modular-mcp](https://github.com/d-kimuson/modular-mcp)
- MCP 规范：[Model Context Protocol](https://modelcontextprotocol.io/)
- Rust MCP 生态：[rust-mcp-stack](https://github.com/rust-mcp-stack)
- 上游仓库：[asyrjasalo/dynamic-mcp](https://github.com/asyrjasalo/dynamic-mcp)（本 fork 基于此仓库的 v1.5.0 分支持续迭代）
