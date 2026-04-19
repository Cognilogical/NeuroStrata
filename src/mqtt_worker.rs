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

                        let cols = 3;
                        let width = 450;
                        let height = 300;
                        let spacing_x = 100;
                        let spacing_y = 150;

                        let mut existing_ids = std::collections::HashSet::new();
                        for m in &all_memories {
                            existing_ids.insert(m.id.clone());
                        }

                        for (i, m) in all_memories.iter().enumerate() {
                            let col = i % cols;
                            let row = i / cols;
                            let x = col as i32 * (width + spacing_x);
                            let y = row as i32 * (height + spacing_y);

                            let domain = m
                                .payload
                                .metadata
                                .get("domain")
                                .and_then(|d| d.as_str())
                                .unwrap_or("general");
                            let r_type = &m.payload.memory_type;
                            let text = format!(
                                "### {}\n**Domain:** {}\n\n{}\n\n---\n*Location:* `{}`",
                                r_type.to_uppercase(),
                                domain,
                                m.payload.content,
                                m.payload.location
                            );

                            nodes.push(serde_json::json!({
                                "id": m.id,
                                "type": "text",
                                "text": text,
                                "x": x,
                                "y": y,
                                "width": width,
                                "height": height,
                                "color": "1"
                            }));

                            if let Some(related) = m
                                .payload
                                .metadata
                                .get("related_to")
                                .and_then(|r| r.as_array())
                            {
                                for target_val in related {
                                    if let Some(target_id) = target_val.as_str() {
                                        if existing_ids.contains(target_id) {
                                            let edge_id = uuid::Uuid::new_v4().to_string();
                                            edges.push(serde_json::json!({
                                                "id": edge_id,
                                                "fromNode": m.id,
                                                "fromSide": "right",
                                                "toNode": target_id,
                                                "toSide": "left",
                                                "color": "4"
                                            }));
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
