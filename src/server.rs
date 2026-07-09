use crate::proxy::types::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use crate::proxy::ModularMcpClient;
use anyhow::Result;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

pub struct ModularMcpServer {
    client: Arc<tokio::sync::RwLock<ModularMcpClient>>,
    name: String,
    version: String,
    subscriptions: Arc<tokio::sync::RwLock<HashSet<String>>>,
}

impl ModularMcpServer {
    pub fn new(
        client: Arc<tokio::sync::RwLock<ModularMcpClient>>,
        name: String,
        version: String,
    ) -> Self {
        Self {
            client,
            name,
            version,
            subscriptions: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
        }
    }

    pub async fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request).await,
            "tools/list" => self.handle_list_tools(request).await,
            "tools/call" => self.handle_call_tool(request).await,
            "resources/list" => self.handle_resources_list(request).await,
            "resources/read" => self.handle_resources_read(request).await,
            "resources/templates/list" => self.handle_resources_templates_list(request).await,
            "resources/subscribe" => self.handle_resources_subscribe(request).await,
            "resources/unsubscribe" => self.handle_resources_unsubscribe(request).await,
            "prompts/list" => self.handle_prompts_list(request).await,
            "prompts/get" => self.handle_prompts_get(request).await,
            _ => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", request.method),
                    data: None,
                }),
            },
        }
    }

    async fn handle_initialize(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {},
                    "resources": {
                        "subscribe": true
                    },
                    "prompts": {}
                },
                "serverInfo": {
                    "name": self.name,
                    "version": self.version
                }
            })),
            error: None,
        }
    }

    async fn handle_list_tools(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let client = self.client.read().await;
        let groups = client.list_groups();
        let failed_groups = client.list_failed_groups();

        let group_names: Vec<String> = groups.iter().map(|g| g.name.clone()).collect();

        let groups_desc = groups
            .iter()
            .map(|g| format!("- {}: {}", g.name, g.description))
            .collect::<Vec<_>>()
            .join("\n");

        let failed_desc = if !failed_groups.is_empty() {
            let failed = failed_groups
                .iter()
                .map(|g| format!("- {}: {} (Error: {})", g.name, g.description, g.error))
                .collect::<Vec<_>>()
                .join("\n");
            format!("\n\nUnavailable groups (connection failed):\n{}", failed)
        } else {
            String::new()
        };

        let get_tools_desc = format!(
            "dynamic-mcp manages multiple MCP servers as organized groups, \
            providing only the necessary group's tool descriptions to the LLM \
            on demand instead of overwhelming it with all tool descriptions at once.\n\n\
            Use this tool to retrieve available tools in a specific group, \
            then use call_dynamic_tool to execute them.\n\n\
            Available groups:\n{}{}",
            groups_desc, failed_desc
        );

        let call_tool_desc = r#"Execute a tool from a specific MCP group. Proxies the call to the appropriate upstream MCP server.

Use get_dynamic_tools first to discover available tools and their input schemas in the specified group, then use this tool to execute them.

This maintains a clean separation between discovery (context-efficient) and execution phases, enabling effective management of large tool collections across multiple MCP servers.

Example usage:
  call_dynamic_tool(group="playwright", name="browser_navigate", args={"url": "https://example.com"})
  → Executes the browser_navigate tool from the playwright group with the specified arguments"#;

        let list_groups_desc = format!(
            "List all available MCP server groups registered with dynamic-mcp. \
            Returns the group name, description, and status for each configured upstream MCP server. \
            Use this tool when you need to discover what MCP services are available \
            before calling get_dynamic_tools or call_dynamic_tool.\n\n\
            Currently available groups:\n{}{}\n\n\
            Returns a JSON object with 'groups' array containing {{name, description, status}} for each server.",
            groups_desc, failed_desc
        );

        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(json!({
                "tools": [
                    {
                        "name": "get_dynamic_tools",
                        "description": get_tools_desc,
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "group": {
                                    "type": "string",
                                    "description": "The name of the MCP group to get tools from",
                                    "enum": group_names
                                }
                            },
                            "required": ["group"]
                        }
                    },
                    {
                        "name": "call_dynamic_tool",
                        "description": call_tool_desc,
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "group": {
                                    "type": "string",
                                    "description": "The name of the MCP group containing the tool",
                                    "enum": group_names
                                },
                                "name": {
                                    "type": "string",
                                    "description": "The name of the tool to execute"
                                },
                                "args": {
                                    "type": "object",
                                    "description": "Arguments to pass to the tool",
                                    "additionalProperties": true
                                }
                            },
                            "required": ["group", "name"]
                        }
                    },
                    {
                        "name": "list_groups",
                        "description": list_groups_desc,
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        }
                    }
                ]
            })),
            error: None,
        }
    }

    async fn handle_call_tool(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let params = request.params.clone().unwrap_or(json!({}));
        let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

        match tool_name {
            "get_dynamic_tools" => {
                let group_name = arguments
                    .get("group")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                if group_name.is_empty() {
                    return JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: request.id,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32602,
                            message: "Missing required parameter: group".to_string(),
                            data: None,
                        }),
                    };
                }

                let client = self.client.read().await;
                match client.get_group_tools(group_name).await {
                    Ok(tools) => {
                        let tools_json: Vec<serde_json::Value> = tools
                            .into_iter()
                            .map(|t| {
                                json!({
                                    "name": t.name,
                                    "description": t.description,
                                    "inputSchema": t.input_schema
                                })
                            })
                            .collect();

                        JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id: request.id,
                            result: Some(json!({
                                "content": [{
                                    "type": "text",
                                    "text": serde_json::to_string(&tools_json).unwrap_or_default()
                                }]
                            })),
                            error: None,
                        }
                    }
                    Err(e) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: request.id,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32603,
                            message: format!("Failed to list tools: {}", e),
                            data: None,
                        }),
                    },
                }
            }
            "call_dynamic_tool" => {
                let group_name = arguments
                    .get("group")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let actual_tool_name = arguments
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let tool_args = arguments.get("args").cloned().unwrap_or(json!({}));

                let client = self.client.read().await;
                match client.call_group_tool(group_name, actual_tool_name, tool_args).await {
                    Ok(result) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: request.id,
                        result: Some(json!({
                            "content": [{
                                "type": "text",
                                "text": result
                            }]
                        })),
                        error: None,
                    },
                    Err(e) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: request.id,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32603,
                            message: format!("Failed to call tool: {}", e),
                            data: None,
                        }),
                    },
                }
            }
            "list_groups" => {
                let client = self.client.read().await;
                let groups = client.list_groups();
                let failed_groups = client.list_failed_groups();

                let all_groups: Vec<serde_json::Value> = groups
                    .iter()
                    .map(|g| {
                        json!({
                            "name": g.name,
                            "description": g.description,
                            "status": "connected"
                        })
                    })
                    .chain(failed_groups.iter().map(|g| {
                        json!({
                            "name": g.name,
                            "description": g.description,
                            "status": "failed",
                            "error": g.error
                        })
                    }))
                    .collect();

                let summary = all_groups
                    .iter()
                    .map(|g| {
                        let name = g["name"].as_str().unwrap_or("");
                        let desc = g["description"].as_str().unwrap_or("");
                        let status = g["status"].as_str().unwrap_or("");
                        format!("- {} ({}): {}", name, status, desc)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: Some(json!({
                        "content": [{
                            "type": "text",
                            "text": format!(
                                "Available dynamic-mcp groups:\n\n{}\n\nUse get_dynamic_tools(group=\"<group_name>\") to see the tools in a specific group.",
                                summary
                            )
                        }]
                    })),
                    error: None,
                }
            }
            _ => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Tool not found: {}", tool_name),
                    data: None,
                }),
            },
        }
    }

    async fn handle_resources_list(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let client = self.client.read().await;
        let all_resources = client.list_all_resources();

        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(json!({
                "resources": all_resources
            })),
            error: None,
        }
    }

    async fn handle_resources_read(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let params = request.params.clone().unwrap_or(json!({}));
        let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");

        let client = self.client.read().await;
        match client.read_resource(uri).await {
            Ok(contents) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: Some(json!({
                    "contents": contents
                })),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32603,
                    message: format!("Failed to read resource: {}", e),
                    data: None,
                }),
            },
        }
    }

    async fn handle_resources_templates_list(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let client = self.client.read().await;
        let templates = client.list_resource_templates();

        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(json!({
                "resourceTemplates": templates
            })),
            error: None,
        }
    }

    async fn handle_resources_subscribe(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let params = request.params.clone().unwrap_or(json!({}));
        let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");

        let mut subscriptions = self.subscriptions.write().await;
        subscriptions.insert(uri.to_string());

        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(json!({})),
            error: None,
        }
    }

    async fn handle_resources_unsubscribe(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let params = request.params.clone().unwrap_or(json!({}));
        let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");

        let mut subscriptions = self.subscriptions.write().await;
        subscriptions.remove(uri);

        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(json!({})),
            error: None,
        }
    }

    async fn handle_prompts_list(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let client = self.client.read().await;
        let prompts = client.list_prompts();

        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(json!({
                "prompts": prompts
            })),
            error: None,
        }
    }

    async fn handle_prompts_get(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let params = request.params.clone().unwrap_or(json!({}));
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let prompt_args = params.get("arguments").cloned().unwrap_or(json!({}));

        let client = self.client.read().await;
        match client.get_prompt(name, prompt_args).await {
            Ok(result) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: Some(result),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32603,
                    message: format!("Failed to get prompt: {}", e),
                    data: None,
                }),
            },
        }
    }
}
