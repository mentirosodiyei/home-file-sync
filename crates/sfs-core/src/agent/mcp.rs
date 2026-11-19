//! Servidor MCP (Model Context Protocol) por entrada/salida estándar.
//!
//! Es lo que arranca la aplicación de IA (Claude Code, Claude Desktop, Codex,
//! Cursor, Gemini CLI, LM Studio…): `home-file-sync --mcp`. Habla JSON-RPC
//! 2.0, un mensaje por línea, y cada herramienta se reenvía a la app abierta
//! (ver [`super::control`]). Si la app no está abierta, la IA recibe un
//! error que lo explica en lugar de un fallo de conexión.

use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// Versiones del protocolo que se entienden (la más reciente primero).
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub type CallFuture = Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;
/// Ejecuta una herramienta: (nombre, argumentos, nombre del cliente).
pub type Caller = Arc<dyn Fn(String, Value, String) -> CallFuture + Send + Sync>;

/// Estado de una sesión MCP.
pub struct Session {
    caller: Caller,
    client: Mutex<String>,
}

impl Session {
    pub fn new(caller: Caller) -> Arc<Session> {
        Arc::new(Session { caller, client: Mutex::new(String::new()) })
    }

    /// Procesa un mensaje. Devuelve la respuesta (nada para notificaciones).
    pub async fn handle(&self, msg: Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let id = match id {
            Some(Value::Null) | None => {
                // Notificaciones (initialized, cancelled…) y respuestas a peticiones nuestras.
                return None;
            }
            Some(id) => id,
        };
        let result = match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
                let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSIONS[0]);
                let name: String = params.pointer("/clientInfo/name").and_then(Value::as_str).unwrap_or("").chars().filter(|c| !c.is_control()).take(60).collect();
                *self.client.lock().unwrap() = name;
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "home-file-sync", "title": "Home File Sync", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": super::api::INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": super::api::tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let args = params.get("arguments").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
                let client = self.client.lock().unwrap().clone();
                let r = (self.caller)(name, args, client).await;
                let (text, is_error) = match r {
                    Ok(t) => (t, false),
                    Err(e) => (e, true),
                };
                Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
            }
            "resources/list" => Ok(json!({ "resources": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            _ => Err((-32601, format!("Method not found: {method}"))),
        };
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        })
    }
}

/// Atiende la sesión por stdin/stdout hasta que se cierre la entrada.
/// Las peticiones se atienden a la vez (una herramienta puede tardar minutos).
pub async fn serve_stdio(session: Arc<Session>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(v) = rx.recv().await {
            let mut line = serde_json::to_vec(&v).unwrap_or_default();
            line.push(b'\n');
            if out.write_all(&line).await.is_err() || out.flush().await.is_err() {
                break;
            }
        }
    });
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.send(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("Parse error: {e}") } }));
                continue;
            }
        };
        // Lotes (versiones antiguas del protocolo).
        let msgs = match msg {
            Value::Array(a) => a,
            m => vec![m],
        };
        for m in msgs {
            let (s, tx) = (session.clone(), tx.clone());
            tokio::spawn(async move {
                if let Some(r) = s.handle(m).await {
                    let _ = tx.send(r);
                }
            });
        }
    }
    drop(tx);
    let _ = writer.await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Arc<Session> {
        Session::new(Arc::new(|name, args, client| Box::pin(async move { if name == "fail" { Err("nope".into()) } else { Ok(format!("{name} {args} {client}")) } })))
    }

    #[tokio::test]
    async fn speaks_mcp() {
        let s = session();
        let r = s
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","clientInfo":{"name":"claude-code","version":"9"},"capabilities":{}}}))
            .await
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "home-file-sync");
        assert!(s.handle(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await.is_none());

        let r = s.handle(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await.unwrap();
        assert!(r["result"]["tools"].as_array().unwrap().len() >= 4);

        let r = s.handle(json!({"jsonrpc":"2.0","id":"x","method":"tools/call","params":{"name":"list_machines","arguments":{"a":1}}})).await.unwrap();
        assert_eq!(r["id"], "x");
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["content"][0]["text"], "list_machines {\"a\":1} claude-code");

        let r = s.handle(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fail"}})).await.unwrap();
        assert_eq!(r["result"]["isError"], true);

        let r = s.handle(json!({"jsonrpc":"2.0","id":4,"method":"nope"})).await.unwrap();
        assert_eq!(r["error"]["code"], -32601);

        let r = s.handle(json!({"jsonrpc":"2.0","id":5,"method":"initialize","params":{"protocolVersion":"2099-01-01"}})).await.unwrap();
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0], "versión desconocida → la más reciente");
    }
}
