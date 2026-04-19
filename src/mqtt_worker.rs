use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::time::Duration;

use crate::traits::{Embedder, MemoryPayload, VectorStore};

#[derive(Deserialize)]
struct MqttRequest {
    request_id: String,
    action: String,
    payload: Value,
}

#[derive(Serialize)]
struct MqttResponse {
    request_id: String,
    success: bool,
    data: Option<Value>,
    error: Option<String>,
}

pub async fn start_worker(embedder: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) {
    let mut mqttoptions = MqttOptions::new("neurostrata-worker", "127.0.0.1", 1883);
    mqttoptions.set_keep_alive(Duration::from_secs(5));

    let (client, mut eventloop) = AsyncClient::new(mqttoptions, 10);

    // Subscribe to all incoming requests
    client
        .subscribe("neurostrata/request", QoS::AtMostOnce)
        .await
        .unwrap();

    tokio::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Incoming::Publish(p))) => {
                    let client_clone = client.clone();
                    let embedder_clone = embedder.clone();
                    let store_clone = store.clone();

                    tokio::spawn(async move {
                        if let Ok(req) = serde_json::from_slice::<MqttRequest>(&p.payload) {
                            let response = handle_request(req, embedder_clone, store_clone).await;
                            let payload = serde_json::to_vec(&response).unwrap();
                            let _ = client_clone
                                .publish("neurostrata/response", QoS::AtMostOnce, false, payload)
                                .await;
                        }
                    });
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("MQTT Worker Error: {:?}", e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });
}

async fn handle_request(
    req: MqttRequest,
    embedder: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> MqttResponse {
    let mut success = false;
    let mut data = None;
    let mut error = None;

    match req.action.as_str() {
        "embed" => {
            if let Some(input) = req.payload.get("input").and_then(|v| v.as_str()) {
                match embedder.embed(input).await {
                    Ok(vec) => {
                        success = true;
                        data = Some(serde_json::json!({ "embedding": vec }));
                    }
                    Err(e) => error = Some(e.to_string()),
                }
            } else {
                error = Some("Missing 'input' field".to_string());
            }
        }
        "list" => {
            let namespace = req
                .payload
                .get("namespace")
                .and_then(|v| v.as_str())
                .unwrap_or("global");
            match store.list(namespace, None).await {
                Ok(results) => {
                    success = true;
                    data = Some(serde_json::to_value(results).unwrap());
                }
                Err(e) => error = Some(e.to_string()),
            }
        }
        "list_namespaces" => match store.list_namespaces().await {
            Ok(namespaces) => {
                success = true;
                data = Some(serde_json::json!(namespaces));
            }
            Err(e) => error = Some(e.to_string()),
        },
        "add" | "update" => {
            if let Ok(memory_req) = serde_json::from_value::<AddMemoryReq>(req.payload.clone()) {
                let namespace = req
                    .payload
                    .get("namespace")
                    .and_then(|v| v.as_str())
                    .unwrap_or("global");
                match store
                    .upsert(
                        namespace,
                        &memory_req.id,
                        memory_req.vector,
                        memory_req.payload,
                    )
                    .await
                {
                    Ok(_) => success = true,
                    Err(e) => error = Some(e.to_string()),
                }
            } else {
                error = Some("Invalid payload format".to_string());
            }
        }
        "delete" => {
            if let Some(id) = req.payload.get("id").and_then(|v| v.as_str()) {
                let namespace = req
                    .payload
                    .get("namespace")
                    .and_then(|v| v.as_str())
                    .unwrap_or("global");
                match store.delete(namespace, id).await {
                    Ok(_) => success = true,
                    Err(e) => error = Some(e.to_string()),
                }
            } else {
                error = Some("Missing 'id' field".to_string());
            }
        }
        "generate_canvas" => {
            if let Some(namespace) = req.payload.get("namespace").and_then(|v| v.as_str()) {
                match store.list(namespace, None).await {
                    Ok(mut all_memories) => {
                        all_memories.retain(|r| {
                            r.payload.metadata.get("valid_to").is_none()
                                || r.payload.metadata["valid_to"].is_null()
                        });

                        let mut nodes = Vec::new();
                        let mut edges = Vec::new();

                        let doc_width = 400;
                        let doc_height = 400;
                        let mem_width = 300;
                        let mem_height = 150;
                        let x_gap = 500;
                        let y_gap = 50;

                        let mut domain_map: std::collections::HashMap<String, Vec<&crate::traits::SearchResult>> = std::collections::HashMap::new();
                        let mut orphaned_memories = Vec::new();

                        for m in &all_memories {
                            let mut found_doc = false;
                            
                            // Check refs first
                            if let Some(refs) = m.payload.metadata.get("refs").and_then(|r| r.as_array()) {
                                for r in refs {
                                    if let Some(file) = r.get("file").and_then(|f| f.as_str()) {
                                        domain_map.entry(file.to_string()).or_default().push(m);
                                        found_doc = true;
                                    }
                                }
                            }
                            
                            // Check location if no refs found
                            if !found_doc && !m.payload.location.is_empty() {
                                domain_map.entry(m.payload.location.clone()).or_default().push(m);
                                found_doc = true;
                            }

                            if !found_doc {
                                orphaned_memories.push(m);
                            }
                        }

                        let start_x = -1000;
                        let start_y = -1000;
                        let mut max_structured_y = start_y + doc_height;

                        // Create sets for fast lookup of related_to
                        let mut existing_ids = std::collections::HashSet::new();
                        for m in &all_memories {
                            existing_ids.insert(m.id.clone());
                        }

                        // Build File Nodes & Grouped Memories
                        let mut col_index = 0;
                        for (doc_path, memories) in &domain_map {
                            let x = start_x + (col_index * (doc_width + x_gap));
                            let y = start_y;
                            let doc_node_id = format!("doc-{}", col_index);

                            nodes.push(serde_json::json!({
                                "id": doc_node_id,
                                "type": "file",
                                "file": doc_path,
                                "x": x,
                                "y": y,
                                "width": doc_width,
                                "height": doc_height,
                                "color": "4"
                            }));

                            for (row_index, m) in memories.iter().enumerate() {
                                let mem_node_id = format!("mem-{}", m.id);
                                let mem_x = x + doc_width + 100;
                                let mem_y = y + (row_index as i32 * (mem_height + y_gap));

                                let domain = m.payload.metadata.get("domain").and_then(|d| d.as_str()).unwrap_or("general");
                                let r_type = &m.payload.memory_type;
                                let text = format!("### {}
**Domain:** {}

{}", r_type.to_uppercase(), domain, m.payload.content);

                                nodes.push(serde_json::json!({
                                    "id": mem_node_id,
                                    "type": "text",
                                    "text": text,
                                    "x": mem_x,
                                    "y": mem_y,
                                    "width": mem_width,
                                    "height": mem_height,
                                    "color": "3"
                                }));

                                edges.push(serde_json::json!({
                                    "id": format!("edge-doc-{}", m.id),
                                    "fromNode": mem_node_id,
                                    "fromSide": "left",
                                    "toNode": doc_node_id,
                                    "toSide": "right",
                                    "color": "5"
                                }));
                                
                                // Related_to edges
                                if let Some(related) = m.payload.metadata.get("related_to").and_then(|r| r.as_array()) {
                                    for target_val in related {
                                        if let Some(target_id) = target_val.as_str() {
                                            if existing_ids.contains(target_id) {
                                                edges.push(serde_json::json!({
                                                    "id": format!("edge-rel-{}-{}", m.id, target_id),
                                                    "fromNode": mem_node_id,
                                                    "fromSide": "right",
                                                    "toNode": format!("mem-{}", target_id),
                                                    "toSide": "left",
                                                    "color": "4"
                                                }));
                                            }
                                        }
                                    }
                                }
                            }

                            let column_max_y = y + (memories.len() as i32 * (mem_height + y_gap));
                            if column_max_y > max_structured_y {
                                max_structured_y = column_max_y;
                            }
                            col_index += 1;
                        }

                        // Orphaned Memories
                        if !orphaned_memories.is_empty() {
                            let orph_start_y = max_structured_y + 400;
                            let cols = 4;
                            let rows = (orphaned_memories.len() as f32 / cols as f32).ceil() as i32;
                            
                            let orph_group_width = (cols * mem_width) + ((cols - 1) * 50) + 100;
                            let orph_group_height = (rows * mem_height) + ((rows - 1) * 50) + 100;

                            nodes.push(serde_json::json!({
                                "id": "group-orphans",
                                "type": "group",
                                "x": start_x - 50,
                                "y": orph_start_y - 50,
                                "width": orph_group_width,
                                "height": orph_group_height,
                                "label": "Orphaned Memories (No document links)",
                                "color": "1"
                            }));

                            for (i, m) in orphaned_memories.iter().enumerate() {
                                let col = (i as i32) % cols;
                                let row = (i as i32) / cols;
                                
                                let domain = m.payload.metadata.get("domain").and_then(|d| d.as_str()).unwrap_or("general");
                                let r_type = &m.payload.memory_type;
                                let text = format!("### {}
**Domain:** {}

{}", r_type.to_uppercase(), domain, m.payload.content);
                                let mem_node_id = format!("mem-{}", m.id);

                                nodes.push(serde_json::json!({
                                    "id": mem_node_id,
                                    "type": "text",
                                    "text": text,
                                    "x": start_x + (col * (mem_width + 50)),
                                    "y": orph_start_y + (row * (mem_height + 50)),
                                    "width": mem_width,
                                    "height": mem_height,
                                    "color": "1"
                                }));
                                
                                // Related_to edges
                                if let Some(related) = m.payload.metadata.get("related_to").and_then(|r| r.as_array()) {
                                    for target_val in related {
                                        if let Some(target_id) = target_val.as_str() {
                                            if existing_ids.contains(target_id) {
                                                edges.push(serde_json::json!({
                                                    "id": format!("edge-rel-{}-{}", m.id, target_id),
                                                    "fromNode": mem_node_id,
                                                    "fromSide": "right",
                                                    "toNode": format!("mem-{}", target_id),
                                                    "toSide": "left",
                                                    "color": "4"
                                                }));
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        let canvas_json = serde_json::json!({
                            "nodes": nodes,
                            "edges": edges
                        });

                        if let Some(home) = dirs::home_dir() {
                            let vault_dir =
                                home.join("Documents").join("NeuroVault").join(namespace);
                            if !vault_dir.exists() {
                                let _ = std::fs::create_dir_all(&vault_dir);
                            }
                            let canvas_path = vault_dir.join("CognitiveMap.canvas");
                            if let Ok(canvas_str) = serde_json::to_string_pretty(&canvas_json) {
                                if std::fs::write(&canvas_path, canvas_str).is_ok() {
                                    success = true;
                                    data = Some(
                                        serde_json::json!({ "path": format!("{}/CognitiveMap.canvas", namespace) }),
                                    );
                                } else {
                                    error = Some("Failed to write .canvas file.".to_string());
                                }
                            } else {
                                error = Some("Failed to serialize canvas JSON.".to_string());
                            }
                        } else {
                            error = Some("Could not determine home directory.".to_string());
                        }
                    }
                    Err(e) => error = Some(e.to_string()),
                }
            } else {
                error = Some("Missing 'namespace' field".to_string());
            }
        }
        _ => error = Some("Unknown action".to_string()),
    }

    MqttResponse {
        request_id: req.request_id,
        success,
        data,
        error,
    }
}

#[derive(Deserialize)]
struct AddMemoryReq {
    id: String,
    vector: Vec<f32>,
    payload: MemoryPayload,
}
