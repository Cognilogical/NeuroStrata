#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder},
    Emitter,
};

#[derive(Serialize, Deserialize, Default)]
struct Config {
    last_project_path: Option<String>,
}

fn get_config_path() -> PathBuf {
    let mut path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push(".config");
    path.push("NeuroStrata");
    fs::create_dir_all(&path).ok();
    path.push("selected-project");
    path
}

fn load_config() -> Config {
    if let Ok(content) = fs::read_to_string(get_config_path()) {
        Config { last_project_path: Some(content.trim().to_string()) }
    } else {
        Config::default()
    }
}

fn save_config(config: &Config) {
    if let Some(ref path) = config.last_project_path {
        let _ = fs::write(get_config_path(), path);
    }
}

#[derive(Serialize, Deserialize)]
struct GraphData {
    nodes: Vec<serde_json::Value>,
    links: Vec<serde_json::Value>,
}

#[tauri::command]
fn log_message(msg: String) {
    println!("FRONTEND LOG: {}", msg);
}

#[tauri::command]
fn get_last_project_path() -> Option<String> {
    let config = load_config();
    config.last_project_path.filter(|p| !p.trim().is_empty())
}

#[tauri::command]
fn save_project_path(path: String) {
    let mut config = load_config();
    config.last_project_path = Some(path);
    save_config(&config);
}

fn ensure_daemon() -> Result<(), String> {
    // Check if daemon is responding
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .map_err(|e| e.to_string())?;

    if client.get("http://127.0.0.1:34343/health").send().is_err() {
        // Not running, spawn it
        println!("MCP Daemon not running. Starting it...");
        let mcp_bin = dirs::home_dir()
            .map(|mut p| {
                p.push(".local/bin/neurostrata-mcp");
                p
            })
            .unwrap_or_else(|| PathBuf::from("neurostrata-mcp"));

        if mcp_bin.exists() {
            std::process::Command::new(&mcp_bin)
                .arg("daemon")
                .spawn()
                .map_err(|e| format!("Failed to spawn daemon: {}", e))?;
            
            // Wait for it to boot
            std::thread::sleep(std::time::Duration::from_secs(2));
        } else {
            return Err(format!("neurostrata-mcp binary not found at {:?}", mcp_bin));
        }
    }
    Ok(())
}

#[tauri::command]
fn ingest_ast(project_path: String) -> Result<String, String> {
    ensure_daemon()?;
    
    let folder_name = std::path::Path::new(&project_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("global");

    let client = reqwest::blocking::Client::new();
    let resp = client.post("http://127.0.0.1:34343/ingest")
        .json(&serde_json::json!({
            "dir": project_path,
            "namespace": folder_name
        }))
        .send()
        .map_err(|e| e.to_string())?;

    if resp.status().is_success() {
        Ok("AST ingested successfully".to_string())
    } else {
        Err(resp.text().unwrap_or_else(|_| "Failed to ingest".to_string()))
    }
}

#[tauri::command]
fn delete_memory(namespace: String, id: String) -> Result<String, String> {
    ensure_daemon()?;
    
    let client = reqwest::blocking::Client::new();
    let resp = client.post("http://127.0.0.1:34343/delete")
        .json(&serde_json::json!({
            "namespace": namespace,
            "id": id
        }))
        .send()
        .map_err(|e| e.to_string())?;

    if resp.status().is_success() {
        Ok("Deleted".to_string())
    } else {
        Err(resp.text().unwrap_or_else(|_| "Failed to delete".to_string()))
    }
}

#[tauri::command]
fn edit_memory(
    old_namespace: String,
    id: String,
    new_namespace: String,
    content: String,
    location: String,
) -> Result<String, String> {
    ensure_daemon()?;

    let client = reqwest::blocking::Client::new();
    let resp = client.post("http://127.0.0.1:34343/edit")
        .json(&serde_json::json!({
            "old_namespace": old_namespace,
            "id": id,
            "new_namespace": new_namespace,
            "content": content,
            "location": location
        }))
        .send()
        .map_err(|e| e.to_string())?;

    if resp.status().is_success() {
        Ok("Edited".to_string())
    } else {
        Err(resp.text().unwrap_or_else(|_| "Failed to edit".to_string()))
    }
}

#[tauri::command]
fn get_graph(project_path: Option<String>) -> Result<GraphData, String> {
    ensure_daemon()?;

    let mut namespace_filter = "global".to_string();
    if let Some(path_str) = &project_path {
        let path = std::path::Path::new(path_str);
        if let Some(folder_name) = path.components().last().and_then(|c| c.as_os_str().to_str()) {
            namespace_filter = folder_name.to_string();
        }
    }
    
    let client = reqwest::blocking::Client::new();
    let resp = client.get("http://127.0.0.1:34343/graph")
        .query(&[("namespace", namespace_filter)])
        .send()
        .map_err(|e| e.to_string())?;

    if resp.status().is_success() {
        let data: GraphData = resp.json().map_err(|e| e.to_string())?;
        Ok(data)
    } else {
        Err(resp.text().unwrap_or_else(|_| "Failed to fetch graph".to_string()))
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            get_graph,
            get_last_project_path,
            save_project_path,
            ingest_ast,
            delete_memory,
            edit_memory,
            log_message
        ])
        .setup(|app| {
            let open_project =
                MenuItemBuilder::with_id("open_project", "Open Project").build(app)?;
            let file_menu = SubmenuBuilder::new(app, "File")
                .item(&open_project)
                .build()?;

            let menu = MenuBuilder::new(app).item(&file_menu).build()?;

            app.set_menu(menu)?;

            let _handle = app.handle().clone();
            app.on_menu_event(move |app, event| {
                println!("Menu event triggered: {:?}", event.id());
                if event.id() == "open_project" {
                    println!("Emitting open-project-dialog event");
                    if let Err(e) = app.emit("open-project-dialog", "open") {
                        println!("Failed to emit event: {}", e);
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}