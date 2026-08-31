use crate::traits::{Embedder, MemoryPayload, VectorStore};
use serde::{Deserialize, Serialize};
use serde_json::{self, Value};
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[allow(dead_code)]
    pub params: Option<Value>,
}

#[derive(Serialize)]
pub struct JsonRpcResponse<T> {
    jsonrpc: String,
    id: Option<Value>,
    // A response carries a result or an error, never both.
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

impl<T> JsonRpcResponse<T> {
    pub fn success(id: Option<Value>, result: T) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: Option<Value>, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(serde_json::json!({ "code": code, "message": message })),
        }
    }
}

/// The request was not JSON.
const PARSE_ERROR: i64 = -32700;
/// The request was JSON-RPC, and answering it failed here rather than at the
/// far end.
const INTERNAL_ERROR: i64 = -32603;

#[allow(dead_code)]
impl JsonRpcResponse<Value> {
    pub fn error(id: Option<Value>, error: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

pub async fn process_mcp_request(
    request: JsonRpcRequest,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
    ingests: Arc<crate::ingest_jobs::IngestJobs>,
) -> Value {
    let id = request.id.clone();
    match request.method.as_str() {
        "initialize" => {
            let result = serde_json::json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": "neurostrata-mcp",
                    "version": "1.0.0"
                },
                "capabilities": {
                    "tools": {}
                }
            });
            serde_json::to_value(JsonRpcResponse::success(id.clone(), result)).unwrap_or_else(|e| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32603,
                        "message": format!("Internal serialization error: {}", e)
                    }
                })
            })
        }
        "notifications/initialized" => {
            serde_json::json!({})
        }
        "tools/list" => {
            let result = serde_json::json!({
                "tools": [
                    {
                        "name": "neurostrata_add_memory",
                        "description": "Store an architectural rule, project pattern, or task insight.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string", "description": "The text of the memory to save." },
                                "namespace": { "type": "string", "description": "The exact project name (e.g., 'NeuroStrata') or 'global'. Do not use folder paths." },
                                "project_root": { "type": "string", "description": "The absolute path to the project root directory where the agent is currently working." },
                                "memory_type": { "type": "string", "description": "Type of memory: 'rule', 'preference', 'bootstrap', 'persona', or 'context'. Defaults to 'context'." },
                                "create_new_namespace": { "type": "boolean", "description": "Set to true ONLY if you are absolutely certain this is a brand new project namespace that doesn't exist yet." },
                                "user_id": { "type": "string", "description": "The user making the request." },
                                "agent_name": { "type": "string", "description": "The name of the agent storing the memory." },
                                "locations": { 
                                    "type": "array", 
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "path": { "type": "string", "description": "File path (e.g. docs/architecture.md)" },
                                            "lines": { "type": "string", "description": "Line numbers (e.g. 42-49)" },
                                            "symbol": { "type": "string", "description": "Code symbol (e.g. startSync())" }
                                        }
                                    },
                                    "description": "An array of file paths, line numbers, and symbols this memory governs. Memories MUST reference the specific documents they belong to."
                                },
                                "domain": { "type": "string", "description": "Optional category or domain this rule belongs to (e.g., 'frontend', 'database', 'devops', 'api')." },
                                "related_to": { "type": "array", "items": { "type": "string" }, "description": "Optional list of memory IDs this rule connects to, forming a knowledge graph edge." },
                                "metadata": { "type": "object", "description": "Optional dictionary with Bi-Directional Anchors" }
                            },
                            "required": ["content", "namespace", "project_root"]
                        }
                    },
                    {
                        "name": "neurostrata_get_memory",
                        "description": "Fetch a single memory by id. Use it to read a memory before editing it, and to follow a Related Nodes or Governs pointer to the exact record instead of guessing at it with a search.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "The id of the memory to fetch." },
                                "namespace": { "type": "string", "description": "The namespace the memory lives in." }
                            },
                            "required": ["id", "namespace"]
                        }
                    },
                    {
                        "name": "neurostrata_get_snapshot",
                        "description": "Get a pre-computed cognitive snapshot of the most important active architectural rules for a project. Use this immediately upon starting a new task to ground yourself in the project's core architecture before searching.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "namespace": { "type": "string", "description": "The exact project name (e.g., 'NeuroStrata') or 'global'." }
                            },
                            "required": ["namespace"]
                        }
                    },
                    {
                        "name": "neurostrata_ingest_directory",
                        "description": "Batch ingest and parse the Abstract Syntax Tree (AST) of the current project directory to build the Software Graph.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "dir_path": { "type": "string", "description": "Absolute path to the project directory to ingest. Usually the current working directory." },
                                "namespace": { "type": "string", "description": "The project namespace." }
                            },
                            "required": ["dir_path", "namespace"]
                        }
                    },
                    {
                        "name": "neurostrata_list_namespaces",
                        "description": "List all existing project namespaces in the database. Use this to prevent hallucinating namespace names.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        }
                    },
                    {
                        "name": "neurostrata_search_memory",
                        "description": "Search the project's long-term memory for architectural rules.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "query": { "type": "string", "description": "What to search for." },
                                "namespace": { "type": "string", "description": "The exact project name (e.g., 'NeuroStrata') or 'global'. Do not use folder paths." }
                            },
                            "required": ["query", "namespace"]
                        }
                    },
                    {
                        "name": "neurostrata_supersede_memory",
                        "description": "Correct a memory that is wrong or out of date. Stores your new text as a new memory and retires the old one, which keeps its original wording as history and stops being returned by search. Re-embeds, so search matches the new wording and not the words you replaced. This is the only tool on this surface that changes an existing memory.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "The id of the memory being corrected. Get it from a search result or neurostrata_get_memory." },
                                "namespace": { "type": "string", "description": "The namespace the memory lives in." },
                                "content": { "type": "string", "description": "The corrected text, written in full. It replaces the old wording rather than being appended to it." },
                                "allow_global": { "type": "boolean", "description": "Required to supersede anything in the machine-wide 'global' namespace, whose rules apply to every project on this machine." }
                            },
                            "required": ["id", "namespace", "content"]
                        }
                    }
                ]
            });
            serde_json::to_value(JsonRpcResponse::success(id.clone(), result)).unwrap_or_else(|e| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32603,
                        "message": format!("Internal serialization error: {}", e)
                    }
                })
            })
        }
        "tools/call" => {
            let mut result_text = "Tool execution failed".to_string();
            if let Some(params) = &request.params {
                if let Some(name) = params.get("name").and_then(|n| n.as_str()) {
                    let arguments = params
                        .get("arguments")
                        .cloned()
                        .unwrap_or(serde_json::json!({}));

                    match name {
                        "neurostrata_list_namespaces" => {
                            result_text = handle_list_namespaces(store.clone()).await;
                        }
                        "neurostrata_add_memory" => {
                            result_text = handle_add_memory(arguments, emb.clone(), store.clone()).await;
                        }
                        "neurostrata_get_memory" => {
                            result_text = handle_get_memory(arguments, store.clone()).await;
                        }
                        "neurostrata_get_snapshot" => {
                            result_text = handle_get_snapshot(arguments, store.clone()).await;
                        }
                        "neurostrata_ingest_directory" => {
                            result_text = handle_ingest_directory(arguments, emb.clone(), store.clone(), ingests.clone()).await;
                        }
                        "neurostrata_search_memory" => {
                            result_text = handle_search_memory(arguments, emb.clone(), store.clone()).await;
                        }
                        "neurostrata_supersede_memory" => {
                            result_text = handle_supersede_memory(arguments, emb.clone(), store.clone()).await;
                        }
                        _ => {
                            result_text = format!("Unknown tool: {}", name);
                        }
                    }
                }
            }

            let result = serde_json::json!({
                "content": [
                    { "type": "text", "text": result_text }
                ]
            });
            serde_json::to_value(JsonRpcResponse::success(id.clone(), result)).unwrap_or_else(|e| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32603,
                        "message": format!("Internal serialization error: {}", e)
                    }
                })
            })
        }
        _ => serde_json::json!({})
    }
}

// Optional proxy helper to keep backwards compat with the stdio loop
/// How long a proxied call may run before the caller is told it failed.
///
/// A blanket 60 seconds used to sit here, which is shorter than the operation
/// that most needs the proxy: a full directory ingest of this repository takes
/// 58 to 86 seconds, so it was cut off every time. The bound is now generous
/// enough for real work and overridable, and -- unlike before -- reaching it
/// produces an answer rather than silence.
fn proxy_timeout() -> std::time::Duration {
    let secs = std::env::var("NEUROSTRATA_PROXY_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(1800);
    std::time::Duration::from_secs(secs)
}

/// How long the proxy will keep waiting for a daemon THIS PROCESS started before
/// it reports the daemon as unreachable. Generous because the first run of a new
/// install downloads the embedding model, which is minutes on a slow link.
/// Irrelevant when the daemon was already up: that case never waits.
fn daemon_startup_budget() -> std::time::Duration {
    let secs = std::env::var("NEUROSTRATA_DAEMON_STARTUP_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(600);
    std::time::Duration::from_secs(secs)
}

/// Whether to keep waiting rather than report the daemon unreachable.
///
/// Pure so it can be tested: the loop it drives needs a live socket, but the
/// decision is four facts and no I/O. Only a CONNECT failure is worth waiting
/// on -- a timeout means something answered and then took too long, and any
/// other error is not going to fix itself by being asked again.
fn should_wait_for_startup(
    daemon_has_answered: bool,
    is_connect_error: bool,
    waited: std::time::Duration,
    budget: std::time::Duration,
) -> bool {
    !daemon_has_answered && is_connect_error && waited < budget
}

/// Which of the two ways this proxy came to exist.
///
/// `neurostrata-mcp` with no arguments is a proxy either way, but not the same
/// proxy: one is talking to a daemon that was already answering, the other to a
/// daemon it has just spawned and that may still be loading its model. A connect
/// failure means opposite things in the two cases, and saying "it is not running"
/// about a daemon this process started thirty seconds ago is simply wrong.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DaemonOrigin {
    /// A daemon answered /health before the proxy started.
    AlreadyRunning,
    /// This process spawned the daemon and did not wait for it to finish.
    SpawnedByUs,
}

/// Writes one JSON-RPC message and flushes it, so the caller sees it now rather
/// than whenever the buffer happens to fill.
async fn write_message(writer: &mut io::Stdout, message: &str) -> io::Result<()> {
    writer.write_all(message.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

/// The reply a failed request gets, or None when it must not get one.
///
/// JSON-RPC notifications carry no id and must never be answered, so a failure
/// on one is reported to stderr and goes no further. Everything else gets an
/// error response: a request read and then dropped leaves the client waiting on
/// an id that never comes back, which is a hang with no error reported
/// anywhere (bead neurostrata-oty).
fn failure_line(id: Option<Value>, code: i64, message: &str) -> Option<String> {
    id.as_ref()?;
    let response = JsonRpcResponse::<Value>::failure(id, code, message.to_string());
    serde_json::to_string(&response).ok()
}

async fn answer_with_error(
    writer: &mut io::Stdout,
    id: Option<Value>,
    code: i64,
    message: String,
) -> io::Result<()> {
    eprintln!("{}", message);
    match failure_line(id, code, &message) {
        Some(line) => write_message(writer, &line).await,
        None => Ok(()),
    }
}

pub async fn start_mcp_proxy(origin: DaemonOrigin) -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let mut writer = stdout;

    let client = reqwest::Client::builder()
        // Separate from the overall bound on purpose. A port with nothing behind
        // it hangs rather than refusing on this machine -- the same behaviour
        // DaemonProbe in main.rs exists to describe -- so without this, a daemon
        // that is not running is indistinguishable from one that is merely slow,
        // and the wait is the whole timeout rather than a few seconds.
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(proxy_timeout())
        .build()
        .unwrap();

    // Whether anything has ever answered on the port. Until something has, a
    // daemon we spawned ourselves is presumed to be still starting; after it
    // has, a connect failure means it went away, which is a different report.
    let mut daemon_has_answered = origin == DaemonOrigin::AlreadyRunning;

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        // Every path from here answers, or deliberately does not because the
        // message was a notification. Reading a request and then dropping it --
        // which is what this loop did whenever the daemon could not be reached,
        // or the line did not parse -- leaves the client waiting on an id that
        // never comes back: a hang with no error reported anywhere.
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(e) => {
                answer_with_error(
                    &mut writer,
                    Some(Value::Null),
                    PARSE_ERROR,
                    format!("The MCP request could not be parsed as JSON: {}", e),
                )
                .await?;
                continue;
            }
        };

        let id = request.get("id").cloned();

        // A daemon this process spawned is not ready the moment it is spawned,
        // and the first request usually arrives before it is. Waiting here is
        // the difference between a session that starts slowly and one that
        // reports every tool as broken.
        let waiting_since = std::time::Instant::now();
        let response = loop {
            let attempt = client
                .post("http://127.0.0.1:34343/mcp")
                .json(&request)
                .send()
                .await;

            let still_starting = should_wait_for_startup(
                daemon_has_answered,
                attempt.as_ref().err().is_some_and(reqwest::Error::is_connect),
                waiting_since.elapsed(),
                daemon_startup_budget(),
            );

            if !still_starting {
                break attempt;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        };

        if response.is_ok() {
            daemon_has_answered = true;
        }

        match response {
            Ok(resp) => {
                let status = resp.status();
                match resp.text().await {
                    Ok(text) if status.is_success() => {
                        // A notification the daemon chose not to answer stays
                        // unanswered here too.
                        if !text.is_empty() {
                            write_message(&mut writer, &text).await?;
                        }
                    }
                    Ok(text) => {
                        answer_with_error(
                            &mut writer,
                            id,
                            INTERNAL_ERROR,
                            format!("The NeuroStrata daemon answered {}: {}", status, text.trim()),
                        )
                        .await?;
                    }
                    Err(e) => {
                        answer_with_error(
                            &mut writer,
                            id,
                            INTERNAL_ERROR,
                            format!("The NeuroStrata daemon's reply could not be read: {}", e),
                        )
                        .await?;
                    }
                }
            }
            Err(e) => {
                let cause = if e.is_connect() && !daemon_has_answered {
                    "this process started one, but it has not begun listening on                      127.0.0.1:34343 within the startup budget -- a first run                      downloads the embedding model, which can take several                      minutes; raise NEUROSTRATA_DAEMON_STARTUP_SECS, or run                      `neurostrata-mcp daemon` in a terminal to watch it start"
                } else if e.is_connect() {
                    "it is not running or is not listening on 127.0.0.1:34343"
                } else if e.is_timeout() {
                    "it did not answer in time; the work may still be running"
                } else {
                    "the request to it failed"
                };
                answer_with_error(
                    &mut writer,
                    id,
                    INTERNAL_ERROR,
                    format!("Could not reach the NeuroStrata daemon: {} ({})", cause, e),
                )
                .await?;
            }
        }
    }
    Ok(())
}



async fn handle_list_namespaces(store: Arc<dyn VectorStore>) -> String {
    if let Ok(namespaces) = store.list_namespaces().await {
        format!("Existing namespaces: {:?}", namespaces)
    } else {
        "Failed to list namespaces.".to_string()
    }
}

// A memory is durable only once checkpointed -- WAL replay restores the catalog
// but not row insertions (bead neurostrata-kug) -- and this surface used to
// checkpoint after every write to close that window. It cost more than it
// bought: the engine waits for all transactions to drain before flushing, so
// under load the checkpoint blocked the writer for minutes and failed anyway
// (bead neurostrata-3fi.6.4). The store now marks itself dirty and the daemon's
// background task does the flushing, which is also what runs at shutdown.

async fn handle_add_memory(arguments: Value, emb: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> String {
    let content = match arguments.get("content").and_then(|c| c.as_str()) {
        Some(c) => c,
        None => return "Missing 'content' parameter.".to_string(),
    };

    let secret_regex = regex::Regex::new(r"(?i)(sk-ant-|ghp_|xoxb-|eyjhbg|api_key\s*=|password\s*=|sk-proj-)").unwrap();
    if secret_regex.is_match(content) {
        return "ERROR [SECURITY]: Memory rejected due to sensitive information (e.g., API keys, passwords, or tokens). Please redact the secrets from your request and try storing the memory again.".to_string();
    }

    let namespace = match arguments.get("namespace").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return "ERROR [NAMESPACE]: 'namespace' is missing. You MUST explicitly provide the specific project namespace. NEVER default to 'global' unless instructed.".to_string(),
    };
    let namespace = resolve_namespace(&store, namespace).await;
    let namespace = namespace.as_str();

    if namespace.contains('/') || namespace.contains('\\') {
        return "ERROR [NAMESPACE]: The namespace cannot be a file path. It must be the exact project name (e.g., 'NeuroStrata'). Do not use slashes.".to_string();
    }

    if namespace != "global" {
        if let Some(project_root) = arguments.get("project_root").and_then(|r| r.as_str()) {
            let ns_dir = std::path::Path::new(project_root).join(".NeuroStrata");
            if !tokio::fs::try_exists(&ns_dir).await.unwrap_or(false) {
                let create_new_namespace = arguments.get("create_new_namespace").and_then(|v| v.as_bool()).unwrap_or(false);
                if !create_new_namespace {
                    return format!("ERROR: No .NeuroStrata directory found at {}. This indicates the project does not have a designated context/namespace yet. Do NOT guess the namespace. Ask the user if they want to initialize this directory as a new context, and if so, call this tool again with create_new_namespace=true.", project_root);
                } else {
                    if let Err(e) = tokio::fs::create_dir_all(&ns_dir).await {
                        return format!("ERROR: Failed to create .NeuroStrata directory: {}", e);
                    }
                }
            }
        }
    }

    let memory_type = arguments.get("memory_type").and_then(|m| m.as_str()).unwrap_or("context");
    let create_new_namespace = arguments.get("create_new_namespace").and_then(|v| v.as_bool()).unwrap_or(false);
    let user_id = arguments.get("user_id").and_then(|u| u.as_str()).unwrap_or("unknown");
    if user_id == "auto-ingestor" {
        // Directory ingestion deletes every row owned by this user_id before it
        // rebuilds, so a memory stored under it would vanish on the next ingest.
        return "ERROR: 'auto-ingestor' is reserved for directory ingestion, and memories stored under it are deleted by the next ingest. Please use a different user_id.".to_string();
    }
    let agent_name = arguments.get("agent_name").and_then(|a| a.as_str()).map(|s| s.to_string());
    let mut location = "".to_string();
    let mut location_lines = "".to_string();
    let mut metadata = arguments.get("metadata").cloned().unwrap_or_else(|| serde_json::json!({}));
    
    if let Some(locations) = arguments.get("locations").and_then(|l| l.as_array()) {
        if let Some(first) = locations.first() {
            location = first.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
            location_lines = first.get("lines").and_then(|l| l.as_str()).unwrap_or("").to_string();
        }
        
        if let Some(obj) = metadata.as_object_mut() {
            let refs: Vec<serde_json::Value> = locations.iter().map(|loc| {
                let mut ref_obj = serde_json::Map::new();
                if let Some(path) = loc.get("path").and_then(|p| p.as_str()) {
                    ref_obj.insert("file".to_string(), serde_json::json!(path));
                }
                if let Some(lines) = loc.get("lines").and_then(|l| l.as_str()) {
                    ref_obj.insert("lines".to_string(), serde_json::json!(lines));
                }
                if let Some(sym) = loc.get("symbol").and_then(|s| s.as_str()) {
                    ref_obj.insert("symbol".to_string(), serde_json::json!(sym));
                }
                serde_json::Value::Object(ref_obj)
            }).collect();
            obj.insert("refs".to_string(), serde_json::Value::Array(refs));

            // A rule that names files governs them. This is the edge that makes an
            // architectural memory reachable from the code it constrains, rather
            // than only from a similar-sounding query.
            let governed: Vec<serde_json::Value> = locations
                .iter()
                .filter_map(|loc| loc.get("path").and_then(|p| p.as_str()))
                .filter(|p| !p.is_empty())
                // Same normalisation the ingester applies to its node ids, so a
                // path written by hand lands on the file node it names.
                .map(crate::parser::ingest::normalize_node_path)
                .fold(Vec::new(), |mut acc: Vec<String>, path| {
                    // Several locations often sit in one file; one edge is enough.
                    if !acc.contains(&path) {
                        acc.push(path);
                    }
                    acc
                })
                .into_iter()
                .map(|p| serde_json::json!(p))
                .collect();
            if !governed.is_empty() {
                obj.insert("governs".to_string(), serde_json::Value::Array(governed));
            }
        }
    }

    if let Some(meta_obj) = metadata.as_object_mut() {
        if let Some(domain) = arguments.get("domain") {
            meta_obj.insert("domain".to_string(), domain.clone());
        }
        if let Some(related_to) = arguments.get("related_to") {
            meta_obj.insert("related_to".to_string(), related_to.clone());
        }
        meta_obj.insert("valid_from".to_string(), serde_json::json!(chrono::Utc::now().timestamp()));
        meta_obj.insert("access_count".to_string(), serde_json::json!(0));
    }

    if let Ok(existing_namespaces) = store.list_namespaces().await {
        if !existing_namespaces.contains(&namespace.to_string()) && !create_new_namespace {
            return format!(
                "Error: Namespace '{}' does not exist. SYSTEM ALERT: Your agent overconfidence and inaccuracy score has been flagged and degraded by the telemetry monitor. You MUST use `neurostrata_list_namespaces` to check existing project names before guessing. Existing namespaces are: {:?}. If you are absolutely certain this is a brand new project, you must explicitly pass `create_new_namespace: true` to bypass this lock.",
                namespace, existing_namespaces
            );
        }
        
        let payload = MemoryPayload {
            content: content.to_string(),
            user_id: user_id.to_string(),
            memory_type: memory_type.to_string(),
            agent_name,
            location,
            location_lines,
            metadata,
        };

        if let Ok(_) = store.init(namespace).await {
            if let Ok(vec) = emb.embed(&content).await {
                let new_id = uuid::Uuid::new_v4().to_string();
                if let Ok(_) = store.upsert(namespace, &new_id, vec, payload).await {
                    return format!("Successfully added memory for namespace: {}", namespace);
                } else {
                    return "Failed to store memory in database.".to_string();
                }
            } else {
                return "Failed to generate embedding.".to_string();
            }
        } else {
            return "Failed to initialize table.".to_string();
        }
    } else {
        return "Failed to verify existing namespaces.".to_string();
    }
}

/// What to tell an agent when this namespace still holds ids written before
/// they carried a namespace, or None when there is nothing to say.
///
/// It rides on get_snapshot because that is the one call an agent is required
/// to make at the start of a task, and because the snapshot has already fetched
/// the list this counts from. `doctor` reports the same condition, but doctor
/// only runs with the daemon stopped, so an agent in a session would otherwise
/// never learn of it -- and an agent is the only party present to raise it.
///
/// The message deliberately hands the work to the user rather than describing
/// something to attempt. Migrating means stopping the daemon, which is the
/// human's transport and would end the session's own connection; an agent that
/// shelled out to do it would be going around the surface it was given.
fn unmigrated_ids_notice(namespace: &str, memories: &[crate::traits::SearchResult]) -> Option<String> {
    let ingested: Vec<&crate::traits::SearchResult> = memories
        .iter()
        .filter(|m| m.payload.user_id == "auto-ingestor")
        .collect();

    let stale = ingested
        .iter()
        .filter(|m| !crate::parser::ingest::is_qualified(namespace, &m.id))
        .count();

    if stale == 0 {
        return None;
    }

    Some(format!(
        "MIGRATION NEEDED -- {} of {} ingested ids in '{}' predate namespace qualification.\n\
         They resolve, but the next project ingesting a shared path takes them.\n\
         Not yours to run: it needs the daemon stopped. Give the user:\n\n  \
         neurostrata-mcp backup <dir>      # daemon still up\n  \
         neurostrata-mcp shutdown\n  \
         neurostrata-mcp ingest <dir> {}\n\n\
         Backup first: re-ingest rewrites every id.",
        stale,
        ingested.len(),
        namespace,
        namespace
    ))
}

async fn handle_get_snapshot(arguments: Value, store: Arc<dyn VectorStore>) -> String {
    let namespace = match arguments.get("namespace").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return "Missing 'namespace' parameter.".to_string(),
    };
    let namespace = resolve_namespace(&store, namespace).await;
    let namespace = namespace.as_str();

    if let Ok(mut all_memories) = store.list(namespace, None).await {
        // Counted from the list the snapshot already had to fetch, so this
        // costs nothing beyond the scan.
        let migration = unmigrated_ids_notice(namespace, &all_memories);

        let now = chrono::Utc::now().timestamp();
        all_memories.retain(|r| {
            match r.payload.metadata.get("valid_to") {
                None => true,
                Some(v) => v.is_null() || (v.as_i64().unwrap_or(0) > now),
            }
        });
        all_memories.sort_by(|a, b| {
            let a_count = a.payload.metadata.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
            let b_count = b.payload.metadata.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
            b_count.cmp(&a_count)
        });
        all_memories.truncate(5);

        let snapshot = if all_memories.is_empty() {
            format!("No active memories found for namespace: {}", namespace)
        } else {
            serde_json::to_string_pretty(&all_memories).unwrap()
        };

        match migration {
            Some(notice) => format!("{}\n\n{}", snapshot, notice),
            None => snapshot,
        }
    } else {
        "Failed to list memories or namespace does not exist.".to_string()
    }
}

async fn handle_ingest_directory(
    arguments: Value,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
    ingests: Arc<crate::ingest_jobs::IngestJobs>,
) -> String {
    let dir_path = match arguments.get("dir_path").and_then(|d| d.as_str()) {
        Some(d) => d,
        None => return "ERROR: dir_path missing.".to_string(),
    };
    let namespace = match arguments.get("namespace").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return "ERROR: namespace missing.".to_string(),
    };
    let namespace = resolve_namespace(&store, namespace).await;
    let namespace = namespace.as_str();

    let schema_str = include_str!("schema.json");
    let schema = match crate::parser::schema::ParserSchema::load(schema_str) {
        Ok(schema) => schema,
        Err(_) => return "Failed to load default parser schema.".to_string(),
    };

    // Handed to the registry so the walk survives this call: an MCP client that
    // times out and disconnects no longer leaves the graph half-built (bead
    // neurostrata-7ej). Ingestion upserts thousands of rows, and checkpointing
    // each one would dominate the run, so that happens once at the end.
    match ingests.run(namespace, dir_path, schema, emb.clone(), store.clone()).await {
        Ok(progress) => format!(
            "Successfully ingested AST from {} into namespace '{}': {} files, {} symbols, {} declared edges relinked.",
            dir_path,
            namespace,
            progress.files_ingested,
            progress.symbols_ingested,
            progress
                .relinked_edges
                .map(|n| n.to_string())
                .unwrap_or_else(|| "no".to_string())
        ),
        Err(e) => format!("Failed to ingest directory {}: {}", dir_path, e),
    }
}

/// Retire a memory and store its replacement, keeping both.
///
/// docs/COGNITIVE_ARCHITECTURE.md section 2 promises that agents never
/// overwrite history: the old node gets a `valid_to` stamp and a new node
/// carries the correction. Only the reading half of that was ever built --
/// `valid_to` was filtered on but never written -- so the only correction paths
/// were destructive rewrites, and `/edit` additionally kept the old embedding.
/// This is the writing half. It adds rather than overwrites, so it is reachable
/// over MCP where edit, delete and move are not.
async fn handle_supersede_memory(
    arguments: Value,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> String {
    let id = match arguments.get("id").and_then(|v| v.as_str()) {
        Some(i) => i,
        None => return "Missing 'id' parameter: supersede needs the memory it replaces.".to_string(),
    };
    let content = match arguments.get("content").and_then(|c| c.as_str()) {
        Some(c) => c,
        None => return "Missing 'content' parameter: supersede needs the corrected text.".to_string(),
    };
    let namespace = match arguments.get("namespace").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return "ERROR [NAMESPACE]: 'namespace' is missing. You MUST explicitly provide the specific project namespace.".to_string(),
    };

    // Same scanner add_memory uses. A correction is still an insertion, and a
    // secret pasted into one would be just as permanent.
    let secret_regex = regex::Regex::new(r"(?i)(sk-ant-|ghp_|xoxb-|eyjhbg|api_key\s*=|password\s*=|sk-proj-)").unwrap();
    if secret_regex.is_match(content) {
        return "ERROR [SECURITY]: Memory rejected due to sensitive information (e.g., API keys, passwords, or tokens). Please redact the secrets from your request and try again.".to_string();
    }

    let namespace = resolve_namespace(&store, namespace).await;
    let namespace = namespace.as_str();

    // Retiring a row hides it from every future search, so the machine-wide
    // stratum keeps the same guard the destructive tools carry.
    if namespace == "global" && !arguments.get("allow_global").and_then(|v| v.as_bool()).unwrap_or(false) {
        return "ERROR [GLOBAL]: Refusing to supersede a memory in the machine-wide 'global' namespace. Rules there apply to every project on this machine. Pass allow_global=true only if you are certain, or supersede the project-local rule instead.".to_string();
    }

    let (old_vector, mut old_payload) = match store.get(namespace, id).await {
        Ok(Some(found)) => found,
        Ok(None) => return format!("No memory with id {} in namespace {}.", id, namespace),
        Err(e) => return format!("Failed to read memory {}: {}", id, e),
    };

    if old_payload
        .metadata
        .get("valid_to")
        .map(|v| !v.is_null())
        .unwrap_or(false)
    {
        return format!(
            "Memory {} was already superseded; it is history. Supersede the memory that replaced it, or add a new one.",
            id
        );
    }

    let now = chrono::Utc::now().timestamp();
    let new_id = uuid::Uuid::new_v4().to_string();

    // The replacement inherits everything the old row established -- who stored
    // it, what it governs, which files it points at -- because a correction to
    // the text is not a change of subject.
    let mut new_payload = old_payload.clone();
    new_payload.content = content.to_string();
    if let Some(obj) = new_payload.metadata.as_object_mut() {
        obj.remove("valid_to");
        obj.insert("valid_from".to_string(), serde_json::json!(now));
        obj.insert("access_count".to_string(), serde_json::json!(0));
        obj.insert("supersedes".to_string(), serde_json::json!(id));
    }

    let vector = match emb.embed(&content).await {
        Ok(v) => v,
        // The whole point of superseding rather than editing: the new text gets
        // its own embedding, so search stops matching the words we replaced.
        Err(e) => return format!("Failed to embed the new content, nothing was changed: {}", e),
    };

    // Write the replacement BEFORE retiring the original. If the second write
    // fails the namespace holds both versions, which is visible and repairable;
    // the other order would leave the rule retired with no successor.
    if let Err(e) = store.upsert(namespace, &new_id, vector, new_payload).await {
        return format!("Failed to store the replacement, nothing was changed: {}", e);
    }

    if let Some(obj) = old_payload.metadata.as_object_mut() {
        obj.insert("valid_to".to_string(), serde_json::json!(now));
        obj.insert("superseded_by".to_string(), serde_json::json!(new_id));
    }
    // Writing the old row back with an empty embedding would have the engine
    // reject it, and the row is deleted before it is re-inserted -- so an empty
    // vector here destroys the history this whole call exists to preserve.
    if old_vector.len() != emb.dimensions() {
        return format!(
            "Stored the replacement as {}, but refused to rewrite {}: its embedding read back as {} floats, expected {}. Both are active.",
            new_id,
            id,
            old_vector.len(),
            emb.dimensions()
        );
    }
    if let Err(e) = store.upsert(namespace, id, old_vector, old_payload).await {
        return format!(
            "Stored the replacement as {}, but failed to retire {}: {}. Both are active; retire the old one from the GUI or CLI.",
            new_id, id, e
        );
    }

    format!(
        "Superseded {} with {} in namespace {}. The old memory keeps its text and is no longer returned by search; it is still readable by id.",
        id, new_id, namespace
    )
}

async fn handle_search_memory(arguments: Value, emb: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> String {
    let query = match arguments.get("query").and_then(|q| q.as_str()) {
        Some(q) => q,
        None => return "Missing 'query' parameter.".to_string(),
    };
    let namespace = match arguments.get("namespace").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return "ERROR [NAMESPACE]: 'namespace' is missing. You MUST explicitly provide the specific project namespace to search in.".to_string(),
    };
    let namespace = resolve_namespace(&store, namespace).await;
    let namespace = namespace.as_str();

    if let Ok(_) = store.init(namespace).await {
        if let Ok(vec) = emb.embed(&query).await {
            if let Ok(results) = store.search(namespace, vec, 5).await {
                if results.is_empty() {
                    "No relevant memories found.".to_string()
                } else {
                    for res in &results {
                        let store_clone = store.clone();
                        let ns_clone = namespace.to_string();
                        let id_clone = res.id.clone();
                        tokio::spawn(async move {
                            let _ = store_clone.increment_access_count(&ns_clone, &id_clone).await;
                        });
                    }

                    let formatted: Vec<String> = results
                        .into_iter()
                        .map(|r| format_memory(&r.id, &r.payload))
                        .collect();
                    formatted.join("\n\n")
                }
            } else {
                "Failed to search database.".to_string()
            }
        } else {
            "Failed to generate embedding for search.".to_string()
        }
    } else {
        "Failed to initialize namespace table.".to_string()
    }
}

/// Resolves a namespace against the ones that already exist, ignoring case.
///
/// A project's identity should not depend on how somebody spelled the folder
/// when they cloned it. This machine's checkout is .../neurostrata while the
/// repository is NeuroStrata; the GUI derives the namespace from the folder
/// name and agents pass the project name, so one project quietly became two
/// strata (bead neurostrata-fld). Windows and macOS being case-insensitive
/// means the same directory can be addressed several ways besides.
///
/// An existing namespace wins on a case-insensitive match and keeps the
/// spelling it was stored with; a genuinely new name is left exactly as the
/// caller wrote it. Listing namespaces costs about two milliseconds, so this
/// runs on every call rather than being cached and going stale.
pub(crate) async fn resolve_namespace(store: &Arc<dyn VectorStore>, requested: &str) -> String {
    match store.list_namespaces().await {
        Ok(existing) => {
            // An exact name always wins. Only when nothing matches exactly does
            // case decide, so a caller naming a namespace that really exists is
            // never redirected to a differently-cased neighbour.
            if existing.iter().any(|known| known == requested) {
                return requested.to_string();
            }
            let mut candidates: Vec<String> = existing
                .into_iter()
                .filter(|known| known.eq_ignore_ascii_case(requested))
                .collect();

            match candidates.len() {
                0 => requested.to_string(),
                1 => candidates.remove(0),
                _ => {
                    // Two spellings already exist. Row order is not a decision,
                    // so take the fuller namespace and say what was passed over.
                    candidates.sort();
                    let mut best = candidates[0].clone();
                    let mut best_len = 0usize;
                    for candidate in &candidates {
                        let held = store.list(candidate, None).await.map(|m| m.len()).unwrap_or(0);
                        if held > best_len {
                            best_len = held;
                            best = candidate.clone();
                        }
                    }
                    eprintln!(
                        "WARNING: '{}' matches {:?}; using '{}', which holds the most memories. Merge them with: neurostrata-mcp move <from> <id> <to>",
                        requested, candidates, best
                    );
                    best
                }
            }
        }
        // A namespace that cannot be verified is used as given: refusing the
        // write would be worse than writing it where the caller asked.
        Err(_) => requested.to_string(),
    }
}

/// One stored anchor as `src/lib.rs:10-20 (parse)`. add_memory stores the path
/// as `file`; `path` is the name the tool's own arguments use.
fn render_anchor(anchor: &Value) -> String {
    let mut out = anchor
        .get("file")
        .or_else(|| anchor.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if let Some(lines) = anchor.get("lines").and_then(|v| v.as_str()).filter(|l| !l.is_empty()) {
        out.push_str(&format!(":{}", lines));
    }
    if let Some(symbol) = anchor.get("symbol").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        out.push_str(&format!(" ({})", symbol));
    }
    out
}

/// One rendering of a memory, shared by search and get, so a record reads the
/// same however the agent reached it.
fn format_memory(id: &str, payload: &MemoryPayload) -> String {
    let mut out = format!(
        "--- Memory ID: {} ---\nType: {}\nContent: {}",
        id, payload.memory_type, payload.content
    );
    if !payload.location.is_empty() {
        out.push_str(&format!("\nFile Location: {}", payload.location));
        if !payload.location_lines.is_empty() {
            out.push_str(&format!(" (Lines: {})", payload.location_lines));
        }
    }
    // add_memory stores every anchor under `refs`, one {file, lines, symbol} each.
    // This used to read `locations`, which nothing writes, so only the first
    // anchor ever showed -- as File Location above, with no symbol.
    if let Some(anchors) = payload
        .metadata
        .get("refs")
        .or_else(|| payload.metadata.get("locations"))
        .and_then(|v| v.as_array())
        .filter(|arr| !arr.is_empty())
    {
        let rendered: Vec<String> = anchors.iter().map(render_anchor).collect();
        out.push_str(&format!("\nCode Graph Locations: {}", rendered.join(", ")));
    }
    for (key, label) in [
        ("related_to", "Related Nodes"),
        ("contained_by", "Contained By"),
        ("governs", "Governs"),
    ] {
        if let Some(value) = payload.metadata.get(key) {
            if let Some(arr) = value.as_array() {
                if !arr.is_empty() {
                    out.push_str(&format!("\n{}: {}", label, value));
                }
            }
        }
    }
    out
}

async fn handle_get_memory(arguments: Value, store: Arc<dyn VectorStore>) -> String {
    let id = match arguments.get("id").and_then(|v| v.as_str()) {
        Some(i) => i,
        None => return "Missing 'id' parameter. Ids are returned by neurostrata_search_memory.".to_string(),
    };
    let namespace = match arguments.get("namespace").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return "ERROR [NAMESPACE]: 'namespace' is missing. You MUST explicitly provide the namespace the memory lives in.".to_string(),
    };
    let namespace = resolve_namespace(&store, namespace).await;
    let namespace = namespace.as_str();
    if namespace.contains('/') || namespace.contains('\\') {
        return "ERROR [NAMESPACE]: The namespace cannot be a file path. It must be the exact project name (e.g., 'NeuroStrata'). Do not use slashes.".to_string();
    }

    match store.get(namespace, id).await {
        Ok(Some((_, payload))) => {
            let store_clone = store.clone();
            let ns_clone = namespace.to_string();
            let id_clone = id.to_string();
            tokio::spawn(async move {
                let _ = store_clone.increment_access_count(&ns_clone, &id_clone).await;
            });
            format_memory(id, &payload)
        }
        Ok(None) => format!(
            "No memory with id '{}' in namespace '{}'. Ids come from neurostrata_search_memory; check the namespace with neurostrata_list_namespaces.",
            id, namespace
        ),
        Err(e) => format!("Failed to read memory '{}': {}", id, e),
    }
}

#[cfg(test)]
mod startup_tests {
    use super::{should_wait_for_startup, DaemonOrigin};
    use std::time::Duration;

    const BUDGET: Duration = Duration::from_secs(600);

    /// The bug this exists for: `neurostrata-mcp` with no daemon spawns one and
    /// starts proxying without waiting for it. Reporting the first request as
    /// "the daemon is not running" describes a daemon this process just started.
    #[test]
    fn a_daemon_we_started_is_waited_for_rather_than_declared_absent() {
        assert!(should_wait_for_startup(
            false,
            true,
            Duration::from_secs(45),
            BUDGET
        ));
    }

    /// A daemon that was already answering is not starting up, so a refusal is
    /// the truth and the caller should hear it now.
    #[test]
    fn a_daemon_that_was_already_up_is_never_waited_for() {
        assert_eq!(DaemonOrigin::AlreadyRunning, DaemonOrigin::AlreadyRunning);
        assert!(!should_wait_for_startup(
            true,
            true,
            Duration::from_secs(1),
            BUDGET
        ));
    }

    /// Once something has answered, a later refusal means the daemon went away.
    /// Waiting on that would turn a real failure into a hang, which is the
    /// behaviour this whole file exists to remove.
    #[test]
    fn a_daemon_that_answered_and_then_vanished_is_reported_not_awaited() {
        assert!(!should_wait_for_startup(
            true,
            true,
            Duration::from_secs(0),
            BUDGET
        ));
    }

    /// The budget is a bound, not a suggestion: past it the caller gets an
    /// answer, because an unanswered request is the failure mode being fixed.
    #[test]
    fn waiting_stops_at_the_budget() {
        assert!(!should_wait_for_startup(
            false,
            true,
            Duration::from_secs(601),
            BUDGET
        ));
    }

    /// Only a connect failure is a daemon that has not opened its port yet. A
    /// timeout means something answered and then took too long; retrying it
    /// would re-run work that may still be running.
    #[test]
    fn only_a_connect_failure_is_worth_waiting_on() {
        assert!(!should_wait_for_startup(
            false,
            false,
            Duration::from_secs(1),
            BUDGET
        ));
    }
}

#[cfg(test)]
mod migration_notice_tests {
    use super::unmigrated_ids_notice;
    use crate::traits::{MemoryPayload, SearchResult};

    fn node(id: &str, user_id: &str) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            score: 0.0,
            payload: MemoryPayload {
                content: String::new(),
                location: String::new(),
                location_lines: String::new(),
                memory_type: "file".to_string(),
                metadata: serde_json::json!({}),
                user_id: user_id.to_string(),
                agent_name: None,
            },
        }
    }

    #[test]
    fn a_migrated_namespace_says_nothing() {
        let memories = vec![
            node("NeuroStrata::src", "auto-ingestor"),
            node("NeuroStrata::src/main.rs", "auto-ingestor"),
        ];
        assert!(unmigrated_ids_notice("NeuroStrata", &memories).is_none());
    }

    /// Hand-written memories are keyed by UUID and always will be. Counting
    /// them as unmigrated would make the notice permanent and therefore
    /// ignorable, which is worse than not having it.
    #[test]
    fn hand_written_memories_are_not_counted() {
        let memories = vec![
            node("550e8400-e29b-41d4-a716-446655440000", "anthomus"),
            node("NeuroStrata::src", "auto-ingestor"),
        ];
        assert!(unmigrated_ids_notice("NeuroStrata", &memories).is_none());
    }

    #[test]
    fn an_unmigrated_namespace_says_how_many_and_what_to_run() {
        let memories = vec![
            node("src", "auto-ingestor"),
            node("README.md", "auto-ingestor"),
            node("NeuroStrata::src/main.rs", "auto-ingestor"),
        ];
        let notice = unmigrated_ids_notice("NeuroStrata", &memories)
            .expect("two of the three ids predate qualification");
        assert!(notice.contains("2 of 3"), "{}", notice);
        assert!(notice.contains("neurostrata-mcp backup"), "{}", notice);
        assert!(notice.contains("neurostrata-mcp ingest"), "{}", notice);
        // The agent must hand this to the user, not attempt it.
        assert!(notice.contains("MIGRATION NEEDED"), "{}", notice);
    }

    #[test]
    fn a_namespace_with_nothing_ingested_says_nothing() {
        assert!(unmigrated_ids_notice("NeuroStrata", &[]).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    /// A request the proxy could not forward is answered, not dropped. Dropping
    /// it left the client waiting on an id that never came back: a hang with no
    /// error anywhere.
    #[test]
    fn a_request_that_cannot_be_forwarded_is_answered() {
        let line = failure_line(Some(serde_json::json!(7)), INTERNAL_ERROR, "daemon unreachable")
            .expect("a request carrying an id gets a reply");
        let parsed: Value = serde_json::from_str(&line).expect("valid JSON-RPC");

        assert_eq!(parsed["id"], serde_json::json!(7));
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["error"]["code"], INTERNAL_ERROR);
        assert_eq!(parsed["error"]["message"], "daemon unreachable");
    }

    /// A response carries a result or an error, never both, and never a null
    /// result beside an error.
    #[test]
    fn an_error_response_carries_no_result() {
        let line = failure_line(Some(serde_json::json!(1)), PARSE_ERROR, "not JSON").unwrap();
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert!(parsed.get("result").is_none(), "got {}", parsed);
    }

    /// Notifications have no id and must never be answered, however badly they
    /// fail.
    #[test]
    fn a_notification_is_never_answered() {
        assert!(failure_line(None, INTERNAL_ERROR, "daemon unreachable").is_none());
    }

    /// 60 seconds used to be the bound, and a directory ingest of this
    /// repository takes 58 to 86 -- so the call that most needs the proxy was
    /// the one it cut off.
    #[test]
    fn the_proxy_bound_outlasts_a_directory_ingest() {
        if std::env::var("NEUROSTRATA_PROXY_TIMEOUT_SECS").is_ok() {
            return;
        }
        assert!(proxy_timeout() > std::time::Duration::from_secs(300));
    }

    fn payload(content: &str) -> MemoryPayload {
        MemoryPayload {
            content: content.to_string(),
            user_id: "tester".to_string(),
            memory_type: "rule".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({}),
        }
    }

    fn namespace_from(existing: &[&str], requested: &str) -> String {
        // The same rule resolve_namespace applies, over a list instead of the store.
        if existing.iter().any(|known| *known == requested) {
            return requested.to_string();
        }
        existing
            .iter()
            .find(|known| known.eq_ignore_ascii_case(requested))
            .map(|known| known.to_string())
            .unwrap_or_else(|| requested.to_string())
    }

    #[test]
    fn a_differently_cased_checkout_lands_in_the_existing_namespace() {
        let existing = ["NeuroStrata", "NeuroPlasticity"];
        assert_eq!(namespace_from(&existing, "neurostrata"), "NeuroStrata");
        assert_eq!(namespace_from(&existing, "NEUROSTRATA"), "NeuroStrata");
    }

    #[test]
    fn an_exact_name_is_never_redirected() {
        // Both spellings exist here, which is the mess this repairs. A caller
        // asking for the one that exists must still reach it.
        let existing = ["NeuroStrata", "neurostrata"];
        assert_eq!(namespace_from(&existing, "neurostrata"), "neurostrata");
        assert_eq!(namespace_from(&existing, "NeuroStrata"), "NeuroStrata");
    }

    #[test]
    fn a_genuinely_new_namespace_keeps_the_spelling_it_was_given() {
        let existing = ["NeuroStrata"];
        assert_eq!(namespace_from(&existing, "SomethingElse"), "SomethingElse");
    }

                                    #[test]
    fn a_formatted_memory_carries_its_anchors() {
        let mut p = payload("checkpoint every write");
        p.location = "src/store/ladybug.rs".to_string();
        p.location_lines = "428-440".to_string();
        p.metadata = serde_json::json!({ "governs": ["src/daemon.rs"] });

        let out = format_memory("abc-123", &p);

        assert!(out.contains("--- Memory ID: abc-123 ---"));
        assert!(out.contains("Content: checkpoint every write"));
        assert!(out.contains("File Location: src/store/ladybug.rs (Lines: 428-440)"));
        assert!(out.contains("Governs: [\"src/daemon.rs\"]"));
    }

    #[test]
    fn every_stored_anchor_is_rendered_with_its_lines_and_symbol() {
        let mut p = payload("parse before storing");
        p.metadata = serde_json::json!({ "refs": [
            { "file": "src/parser/ingest.rs", "lines": "133-160", "symbol": "ingest_directory" },
            { "file": "src/store/ladybug.rs" }
        ]});

        let out = format_memory("abc-123", &p);

        assert!(out.contains("src/parser/ingest.rs:133-160 (ingest_directory)"), "{}", out);
        assert!(out.contains("src/store/ladybug.rs"), "{}", out);
    }

    #[test]
    fn an_empty_anchor_list_is_not_rendered() {
        let mut p = payload("a rule with no edges");
        p.metadata = serde_json::json!({ "governs": [], "related_to": [] });

        let out = format_memory("abc-123", &p);

        assert!(!out.contains("Governs"));
        assert!(!out.contains("Related Nodes"));
    }

    /// Embeds deterministically from the text, so two different strings can
    /// never collide. That is the whole point of the assertions below.
    struct StubEmbedder;

    #[async_trait::async_trait]
    impl Embedder for StubEmbedder {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let mut v = vec![0.0f32; 4];
            for (i, b) in text.bytes().enumerate() {
                v[i % 4] += b as f32;
            }
            Ok(v)
        }
        fn dimensions(&self) -> usize {
            4
        }
    }

    async fn store_with_one_rule(namespace: &str) -> (Arc<dyn VectorStore>, String) {
        let dir = std::env::temp_dir().join(format!("ns-supersede-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::ladybug::LadybugStore::new(&dir, 4).expect("open temp database"));
        store.init(namespace).await.expect("create the schema");
        let id = uuid::Uuid::new_v4().to_string();
        let vector = StubEmbedder.embed("always use podman").await.unwrap();
        store
            .upsert(namespace, &id, vector, payload("always use podman"))
            .await
            .expect("seed the rule");
        (store, id)
    }

    fn new_id_from(message: &str) -> String {
        // "Superseded <old> with <new> in namespace <ns>."
        message
            .split(" with ")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .expect("the reply names the replacement")
            .to_string()
    }

    /// The bi-temporal contract: the old wording survives, stamped with the
    /// moment it stopped being true, and the correction is a separate row.
    #[tokio::test]
    async fn superseding_retires_the_old_row_and_keeps_its_text() {
        let (store, old_id) = store_with_one_rule("probe").await;
        let emb: Arc<dyn Embedder> = Arc::new(StubEmbedder);

        let reply = handle_supersede_memory(
            serde_json::json!({ "id": old_id, "namespace": "probe", "content": "always use docker" }),
            emb,
            store.clone(),
        )
        .await;
        assert!(reply.starts_with("Superseded"), "got {}", reply);

        let (_, old) = store.get("probe", &old_id).await.unwrap().expect("still readable by id");
        assert_eq!(old.content, "always use podman", "history keeps its own wording");
        assert!(old.metadata["valid_to"].as_i64().is_some(), "retired");
        assert_eq!(old.metadata["superseded_by"], serde_json::json!(new_id_from(&reply)));

        let (_, new) = store
            .get("probe", &new_id_from(&reply))
            .await
            .unwrap()
            .expect("the replacement was stored");
        assert_eq!(new.content, "always use docker");
        assert_eq!(new.metadata["supersedes"], serde_json::json!(old_id));
        assert!(new.metadata["valid_from"].as_i64().is_some());
        assert!(
            new.metadata.get("valid_to").map(|v| v.is_null()).unwrap_or(true),
            "the replacement is current, not history"
        );
    }

    /// The defect that made editing useless: the row kept the vector of text it
    /// no longer contained, so search matched the wording we replaced.
    #[tokio::test]
    async fn the_replacement_is_embedded_from_its_own_text() {
        let (store, old_id) = store_with_one_rule("probe").await;
        let emb: Arc<dyn Embedder> = Arc::new(StubEmbedder);

        let reply = handle_supersede_memory(
            serde_json::json!({ "id": old_id, "namespace": "probe", "content": "always use docker" }),
            emb,
            store.clone(),
        )
        .await;

        let (old_vector, _) = store.get("probe", &old_id).await.unwrap().unwrap();
        let (new_vector, _) = store.get("probe", &new_id_from(&reply)).await.unwrap().unwrap();
        assert_ne!(old_vector, new_vector, "the correction carries its own embedding");
        assert_eq!(new_vector, StubEmbedder.embed("always use docker").await.unwrap());
    }

    /// History is not a chain to rewrite: correct the current row instead.
    #[tokio::test]
    async fn a_retired_memory_cannot_be_superseded_again() {
        let (store, old_id) = store_with_one_rule("probe").await;
        let emb: Arc<dyn Embedder> = Arc::new(StubEmbedder);
        let args = serde_json::json!({ "id": old_id, "namespace": "probe", "content": "second" });

        handle_supersede_memory(args.clone(), emb.clone(), store.clone()).await;
        let again = handle_supersede_memory(args, emb, store).await;
        assert!(again.contains("already superseded"), "got {}", again);
    }

    /// Retiring a row hides it from every project on the machine, so the global
    /// stratum takes the same guard the destructive tools carry.
    #[tokio::test]
    async fn the_global_namespace_needs_an_explicit_flag() {
        let (store, old_id) = store_with_one_rule("global").await;
        let emb: Arc<dyn Embedder> = Arc::new(StubEmbedder);

        let refused = handle_supersede_memory(
            serde_json::json!({ "id": old_id, "namespace": "global", "content": "changed" }),
            emb.clone(),
            store.clone(),
        )
        .await;
        assert!(refused.contains("ERROR [GLOBAL]"), "got {}", refused);

        let (_, untouched) = store.get("global", &old_id).await.unwrap().unwrap();
        assert!(untouched.metadata.get("valid_to").is_none(), "nothing was retired");

        let allowed = handle_supersede_memory(
            serde_json::json!({ "id": old_id, "namespace": "global", "content": "changed", "allow_global": true }),
            emb,
            store,
        )
        .await;
        assert!(allowed.starts_with("Superseded"), "got {}", allowed);
    }

    /// A correction is still an insertion, so it meets the same scanner.
    #[tokio::test]
    async fn a_secret_in_the_correction_is_refused() {
        let (store, old_id) = store_with_one_rule("probe").await;
        let emb: Arc<dyn Embedder> = Arc::new(StubEmbedder);

        let refused = handle_supersede_memory(
            serde_json::json!({ "id": old_id, "namespace": "probe", "content": "use ghp_aaaabbbbccccdddd" }),
            emb,
            store.clone(),
        )
        .await;
        assert!(refused.contains("ERROR [SECURITY]"), "got {}", refused);

        let (_, untouched) = store.get("probe", &old_id).await.unwrap().unwrap();
        assert!(untouched.metadata.get("valid_to").is_none(), "nothing was retired");
    }
}
