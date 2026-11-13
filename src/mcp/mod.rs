//! Model Context Protocol (MCP) server for harbour over stdio.

pub mod search;

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use serde_json::{Value, json};

#[cfg(test)]
use self::search::SearchResult;
pub use self::search::{LiveSearch, SearchHandler};

/// Registered MCP tools.
pub struct Tools {
    pub search: Arc<dyn SearchHandler>,
}

impl Tools {
    /// Constructs tools with an explicit search handler (used for test mocking).
    #[cfg(test)]
    pub fn new(search: Arc<dyn SearchHandler>) -> Self {
        Self { search }
    }

    /// Constructs tools wired to live indexer and engine components.
    pub async fn live() -> Self {
        Self {
            search: Arc::new(LiveSearch::new().await),
        }
    }
}

/// Runs the MCP stdio server on the provided input and output streams.
pub async fn run_stdio<R: BufRead, W: Write>(input: R, out: W) -> io::Result<()> {
    let tools = Tools::live().await;
    serve(input, out, &tools).await
}

/// Serves JSON-RPC 2.0 requests over newline-delimited reader/writer streams.
pub async fn serve<R: BufRead, W: Write>(input: R, mut out: W, tools: &Tools) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
            send_error(&mut out, Value::Null, -32700, "Parse error")?;
            continue;
        };

        let Some(req) = val.as_object() else {
            send_error(&mut out, Value::Null, -32700, "Parse error")?;
            continue;
        };

        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params");

        // Per JSON-RPC 2.0 and MCP spec: notifications never receive a reply.
        let Some(id) = req.get("id").cloned() else {
            continue;
        };
        if method == "notifications/initialized" {
            continue;
        }

        match method {
            "initialize" => {
                let resp = handle_initialize(id, params);
                send_response(&mut out, &resp)?;
            }
            "ping" => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {}
                });
                send_response(&mut out, &resp)?;
            }
            "tools/list" => {
                let resp = handle_tools_list(id);
                send_response(&mut out, &resp)?;
            }
            "tools/call" => {
                let resp = handle_tools_call(id, params, tools).await;
                send_response(&mut out, &resp)?;
            }
            _ => {
                send_error(&mut out, id, -32601, "Method not found")?;
            }
        }
    }
    Ok(())
}

fn handle_initialize(id: Value, params: Option<&Value>) -> Value {
    let client_ver = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or("");

    // Negotiate supported protocol versions, defaulting to 2025-06-18.
    let negotiated = match client_ver {
        "2025-06-18" | "2025-03-26" | "2024-11-05" => client_ver,
        _ => "2025-06-18",
    };

    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": negotiated,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "harbour",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    })
}

fn handle_tools_list(id: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "tools": [
                {
                    "name": "search",
                    "description": "Search curated torrents across configured sources",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "Search query terms"
                            },
                            "limit": {
                                "type": "integer",
                                "description": "Max results to return (default 20, max 100)"
                            }
                        },
                        "required": ["query"]
                    }
                }
            ]
        }
    })
}

async fn handle_tools_call(id: Value, params: Option<&Value>, tools: &Tools) -> Value {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let args = params.and_then(|p| p.get("arguments"));

    if name != "search" {
        return tool_error(id, format!("unknown tool: {name}"));
    }

    let query = args
        .and_then(|a| a.get("query"))
        .and_then(Value::as_str)
        .unwrap_or("");

    // Curated top lists remain a TUI feature; empty searches over MCP are rejected.
    if query.trim().is_empty() {
        return tool_error(id, "search query cannot be empty");
    }

    let limit = args
        .and_then(|a| a.get("limit"))
        .and_then(Value::as_i64)
        .map(|l| (l.max(1) as usize).min(100))
        .unwrap_or(20);

    match tools.search.search(query).await {
        Ok(results) => {
            let limited: Vec<_> = results.into_iter().take(limit).collect();
            let text = serde_json::to_string(&limited).unwrap_or_else(|_| "[]".to_string());
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": text
                        }
                    ],
                    "isError": false
                }
            })
        }
        Err(err) => tool_error(id, format!("search error: {err}")),
    }
}

fn tool_error(id: Value, message: impl AsRef<str>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [
                {
                    "type": "text",
                    "text": message.as_ref()
                }
            ],
            "isError": true
        }
    })
}

fn send_error<W: Write>(out: &mut W, id: Value, code: i32, message: &str) -> io::Result<()> {
    let resp = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    });
    send_response(out, &resp)
}

fn send_response<W: Write>(out: &mut W, val: &Value) -> io::Result<()> {
    let s =
        serde_json::to_string(val).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    writeln!(out, "{s}")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;

    use super::*;

    struct MockSearch(Vec<SearchResult>);

    impl SearchHandler for MockSearch {
        fn search<'a>(
            &'a self,
            _query: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SearchResult>, String>> + Send + 'a>> {
            let res = self.0.clone();
            Box::pin(async move { Ok(res) })
        }
    }

    struct FailingSearch(String);

    impl SearchHandler for FailingSearch {
        fn search<'a>(
            &'a self,
            _query: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SearchResult>, String>> + Send + 'a>> {
            let err = self.0.clone();
            Box::pin(async move { Err(err) })
        }
    }

    async fn run_rpc(input: &str, tools: &Tools) -> Vec<Value> {
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, tools).await.unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    #[tokio::test]
    async fn initialize_shape_and_version_negotiation() {
        let tools = Tools::new(Arc::new(MockSearch(Vec::new())));

        for (requested, expected) in [
            ("2025-06-18", "2025-06-18"),
            ("2025-03-26", "2025-03-26"),
            ("2024-11-05", "2024-11-05"),
            ("2023-01-01", "2025-06-18"),
        ] {
            let req = format!(
                concat!(
                    r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","#,
                    r#""params":{{"protocolVersion":"{}"}}}}"#,
                ),
                requested
            );
            let responses = run_rpc(&req, &tools).await;
            assert_eq!(responses.len(), 1);
            let res = &responses[0];
            assert_eq!(res["jsonrpc"], "2.0");
            assert_eq!(res["id"], 1);
            assert_eq!(res["result"]["protocolVersion"], expected);
            assert_eq!(res["result"]["capabilities"]["tools"], json!({}));
            assert_eq!(res["result"]["serverInfo"]["name"], "harbour");
            assert_eq!(
                res["result"]["serverInfo"]["version"],
                env!("CARGO_PKG_VERSION")
            );
        }
    }

    #[tokio::test]
    async fn notification_gets_no_reply() {
        let tools = Tools::new(Arc::new(MockSearch(Vec::new())));
        let input = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
                     {\"jsonrpc\":\"2.0\",\"method\":\"custom/notify\"}\n";
        let responses = run_rpc(input, &tools).await;
        assert!(
            responses.is_empty(),
            "notifications must never be replied to"
        );
    }

    #[tokio::test]
    async fn ping_returns_empty_result() {
        let tools = Tools::new(Arc::new(MockSearch(Vec::new())));
        let req = r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#;
        let responses = run_rpc(req, &tools).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["jsonrpc"], "2.0");
        assert_eq!(responses[0]["id"], 7);
        assert_eq!(responses[0]["result"], json!({}));
    }

    #[tokio::test]
    async fn tools_list_lists_exactly_search_requiring_query() {
        let tools = Tools::new(Arc::new(MockSearch(Vec::new())));
        let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let responses = run_rpc(req, &tools).await;
        assert_eq!(responses.len(), 1);

        let tool_list = responses[0]["result"]["tools"].as_array().unwrap();
        assert_eq!(tool_list.len(), 1);
        assert_eq!(tool_list[0]["name"], "search");

        let schema = &tool_list[0]["inputSchema"];
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "query"));
    }

    #[tokio::test]
    async fn unknown_method_returns_32601() {
        let tools = Tools::new(Arc::new(MockSearch(Vec::new())));
        let req = r#"{"jsonrpc":"2.0","id":42,"method":"unknown_rpc"}"#;
        let responses = run_rpc(req, &tools).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["id"], 42);
        assert_eq!(responses[0]["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn bad_json_returns_32700_with_id_null() {
        let tools = Tools::new(Arc::new(MockSearch(Vec::new())));
        let req = "{ bad json ";
        let responses = run_rpc(req, &tools).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["id"], Value::Null);
        assert_eq!(responses[0]["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn tools_call_search_with_injected_results() {
        let sample_hits: Vec<SearchResult> = (0..150)
            .map(|i| SearchResult {
                name: format!("Item {i}"),
                info_hash: format!("{i:040x}"),
                size_bytes: 1000 + i as u64,
                seeders: 100 - (i as u32 % 100),
                leechers: 5,
                source: "1337x".into(),
                magnet: Some(format!("magnet:?xt=urn:btih:{i:040x}")),
            })
            .collect();

        let tools = Tools::new(Arc::new(MockSearch(sample_hits)));

        // 1. Limit respected (requested 5 of 150)
        let req_limit = concat!(
            r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"search","#,
            r#""arguments":{"query":"test","limit":5}}}"#,
        );
        let res = &run_rpc(req_limit, &tools).await[0];
        assert_eq!(res["result"]["isError"], false);
        let parsed: Vec<SearchResult> =
            serde_json::from_str(res["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(parsed.len(), 5);

        // 2. Default limit is 20
        let req_default = concat!(
            r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"search","#,
            r#""arguments":{"query":"test"}}}"#,
        );
        let res = &run_rpc(req_default, &tools).await[0];
        let parsed: Vec<SearchResult> =
            serde_json::from_str(res["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(parsed.len(), 20);

        // 3. Max 100 clamp (requested 120, capped at 100)
        let req_clamp = concat!(
            r#"{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"search","#,
            r#""arguments":{"query":"test","limit":120}}}"#,
        );
        let res = &run_rpc(req_clamp, &tools).await[0];
        let parsed: Vec<SearchResult> =
            serde_json::from_str(res["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(parsed.len(), 100);

        // 4. Empty query -> isError true (not a protocol error)
        let req_empty = concat!(
            r#"{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"search","#,
            r#""arguments":{"query":"   "}}}"#,
        );
        let res = &run_rpc(req_empty, &tools).await[0];
        assert_eq!(res["result"]["isError"], true);
        assert!(res.get("error").is_none());
        assert!(
            res["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("empty")
        );
    }

    #[tokio::test]
    async fn search_failure_is_error_true_not_protocol_error() {
        let tools = Tools::new(Arc::new(FailingSearch(
            "indexer unreachable: timeout".into(),
        )));
        let req = concat!(
            r#"{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"search","#,
            r#""arguments":{"query":"failing"}}}"#,
        );
        let responses = run_rpc(req, &tools).await;
        assert_eq!(responses.len(), 1);
        let res = &responses[0];
        assert_eq!(res["id"], 14);
        assert!(res.get("error").is_none(), "must not be a protocol error");
        assert_eq!(res["result"]["isError"], true);
        assert!(
            res["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("indexer unreachable")
        );
    }
}
