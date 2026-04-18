use serde::{Deserialize, Serialize};
use serde_json::{self, Value};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Arc;
use crate::traits::{Embedder, VectorStore};

#[derive(Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Serialize)]
struct JsonRpcResponse<T> {
    jsonrpc: String,
    id: Option<Value>,
    result: T,
    error: Option<Value>,
}
impl<T> JsonRpcResponse<T> {
    fn success(id: Option<Value>, result: T) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result,
            error: None,
        }
    }

    fn error(id: Option<Value>, error: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: serde_json::json!({}),
            error: Some(error),
        }
    }
}

pub async fn start_mcp_server(emb: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let mut writer = stdout; // Use unbuffered writer for stdout JSON-RPC

    while let Some(line) = reader.next_line().await? {
        if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(&line) {
            let id = request.id.clone();
            match request.method.as_str() {
                "initialize" => {
                    let result = serde_json::json!({ "status": "initialized" });
                    let resp = JsonRpcResponse::success(id, result);
                    writer.write_all(resp.json().as_bytes());// assistance