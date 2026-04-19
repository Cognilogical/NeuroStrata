use rumqttc::{AsyncClient, MqttOptions, QoS, Event, Incoming};
use std::sync::Arc;
use tokio::time::Duration;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::traits::{Embedder, VectorStore, MemoryPayload};

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
    client.subscribe("neurostrata/request", QoS::AtMostOnce).await.unwrap();
    
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
                            let _ = client_clone.publish("neurostrata/response", QoS::AtMostOnce, false, payload).await;
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

async fn handle_request(req: MqttRequest, embedder: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> MqttResponse {
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
            let namespace = req.payload.get("namespace").and_then(|v| v.as_str()).unwrap_or("global");
            match store.list(namespace, None).await {
                Ok(results) => {
                    success = true;
                    data = Some(serde_json::to_value(results).unwrap());
                }
                Err(e) => error = Some(e.to_string()),
            }
        }
        "add" | "update" => {
            if let Ok(memory_req) = serde_json::from_value::<AddMemoryReq>(req.payload.clone()) {
                let namespace = req.payload.get("namespace").and_then(|v| v.as_str()).unwrap_or("global");
                match store.upsert(namespace, &memory_req.id, memory_req.vector, memory_req.payload).await {
                    Ok(_) => success = true,
                    Err(e) => error = Some(e.to_string()),
                }
            } else {
                error = Some("Invalid payload format".to_string());
            }
        }
        "delete" => {
            if let Some(id) = req.payload.get("id").and_then(|v| v.as_str()) {
                let namespace = req.payload.get("namespace").and_then(|v| v.as_str()).unwrap_or("global");
                match store.delete(namespace, id).await {
                    Ok(_) => success = true,
                    Err(e) => error = Some(e.to_string()),
                }
            } else {
                error = Some("Missing 'id' field".to_string());
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
