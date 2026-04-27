// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder},
    Emitter, Manager,
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

#[tauri::command]
fn ingest_ast(project_path: String) -> Result<String, String> {
    let folder_name = std::path::Path::new(&project_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("global");

    let mcp_bin = dirs::home_dir()
        .map(|mut p| {
            p.push(".local/bin/neurostrata-mcp");
            p
        })
        .unwrap_or_else(|| PathBuf::from("neurostrata-mcp"));

    if mcp_bin.exists() {
        match std::process::Command::new(&mcp_bin)
            .arg("ingest")
            .arg(&project_path)
            .arg(folder_name)
            .output()
        {
            Ok(output) => {
                if output.status.success() {
                    Ok("AST ingested successfully".to_string())
                } else {
                    Err(String::from_utf8_lossy(&output.stderr).to_string())
                }
            }
            Err(e) => Err(format!("Failed to execute neurostrata-mcp: {}", e)),
        }
    } else {
        Err(format!("neurostrata-mcp binary not found at {:?}", mcp_bin))
    }
}

#[tauri::command]
fn delete_memory(namespace: String, id: String) -> Result<String, String> {
    let mcp_bin = dirs::home_dir()
        .map(|mut p| {
            p.push(".local/bin/neurostrata-mcp");
            p
        })
        .unwrap_or_else(|| PathBuf::from("neurostrata-mcp"));

    if mcp_bin.exists() {
        match std::process::Command::new(&mcp_bin)
            .arg("delete")
            .arg(&namespace)
            .arg(&id)
            .output()
        {
            Ok(output) => {
                if output.status.success() {
                    Ok("Memory deleted successfully".to_string())
                } else {
                    Err(String::from_utf8_lossy(&output.stderr).to_string())
                }
            }
            Err(e) => Err(format!("Failed to execute neurostrata-mcp: {}", e)),
        }
    } else {
        Err(format!("neurostrata-mcp binary not found at {:?}", mcp_bin))
    }
}

#[tauri::command]
fn edit_memory(
    old_namespace: String, 
    id: String, 
    new_namespace: String, 
    content: String, 
    location: String
) -> Result<String, String> {
    let mcp_bin = dirs::home_dir()
        .map(|mut p| {
            p.push(".local/bin/neurostrata-mcp");
            p
        })
        .unwrap_or_else(|| PathBuf::from("neurostrata-mcp"));

    if mcp_bin.exists() {
        match std::process::Command::new(&mcp_bin)
            .arg("edit")
            .arg(&old_namespace)
            .arg(&id)
            .arg(&new_namespace)
            .arg(&content)
            .arg(&location)
            .output()
        {
            Ok(output) => {
                if output.status.success() {
                    Ok("Memory edited successfully".to_string())
                } else {
                    Err(String::from_utf8_lossy(&output.stderr).to_string())
                }
            }
            Err(e) => Err(format!("Failed to execute neurostrata-mcp: {}", e)),
        }
    } else {
        Err(format!("neurostrata-mcp binary not found at {:?}", mcp_bin))
    }
}

#[derive(Serialize)]
struct GraphData {
    nodes: Vec<serde_json::Value>,
    links: Vec<serde_json::Value>,
}

#[tauri::command]
fn get_graph(project_path: Option<String>) -> Result<GraphData, String> {
    println!("get_graph called with project_path: {:?}", project_path);
    let db_path = dirs::home_dir()
        .map(|mut p| { p.push(".config/NeuroStrata/data/db"); p })
        .unwrap_or_else(|| PathBuf::from(".config/NeuroStrata/data/db"));

    let config = lbug::SystemConfig::default().read_only(true);
    let db = match lbug::Database::new(&db_path, config) {
        Ok(db) => db,
        Err(e) => {
            println!("Failed to open Kuzu DB: {}", e);
            return Err(e.to_string());
        }
    };
    let conn = lbug::Connection::new(&db)
        .map_err(|e| e.to_string())?;

    let mut namespace_filter = "global".to_string();
    if let Some(path_str) = &project_path {
        let path = std::path::Path::new(path_str);
        if let Some(folder_name) = path.components().last().and_then(|c| c.as_os_str().to_str()) {
            namespace_filter = folder_name.to_string();
        }
    }
    println!("Using namespace_filter: {}", namespace_filter);

    let mut nodes = Vec::new();
    
    // Fetch nodes (only global and the specific project namespace)
    let query_nodes = format!(
        "MATCH (n:Memory) WHERE n.namespace = 'global' OR n.namespace = '{}' 
         RETURN n.id, n.namespace, n.memory_type, n.content, n.location, n.metadata",
        namespace_filter.replace("'", "''")
    );
    
    if let Ok(mut result_nodes) = conn.query(&query_nodes) {
        while let Some(row) = result_nodes.next() {
            let id = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
            let namespace = if let lbug::Value::String(s) = &row[1] { s.clone() } else { "global".to_string() };
            let memory_type = if let lbug::Value::String(s) = &row[2] { s.clone() } else { "unknown".to_string() };
            let content = if let lbug::Value::String(s) = &row[3] { s.clone() } else { "".to_string() };
            let location = if let lbug::Value::String(s) = &row[4] { s.clone() } else { "".to_string() };
            
            let mut metadata_obj = serde_json::Value::Null;
            if let lbug::Value::String(metadata_str) = &row[5] {
                metadata_obj = serde_json::from_str(metadata_str).unwrap_or(serde_json::Value::Null);
            }
            
            let mut absolute_path = "".to_string();
            if let Some(abs_path) = metadata_obj.get("absolute_path").and_then(|v| v.as_str()) {
                absolute_path = abs_path.to_string();
            } else if !location.is_empty() {
                let p = std::path::Path::new(&location);
                if p.is_absolute() {
                    absolute_path = location.clone();
                } else if let Some(pp) = &project_path {
                    let mut full = PathBuf::from(pp);
                    full.push(p);
                    absolute_path = full.to_string_lossy().to_string();
                }
            }

            nodes.push(serde_json::json!({
                "id": id,
                "name": id.split('/').last().unwrap_or(&id).split('\\').last().unwrap_or(&id),
                "namespace": namespace,
                "memory_type": memory_type,
                "content": content,
                "location": location,
                "absolute_path": absolute_path,
                "metadata": metadata_obj
            }));
        }
    }
    
    println!("get_graph returning {} nodes for namespace {}", nodes.len(), namespace_filter);

    let mut links = Vec::new();
    
    let query_edges = format!(
        "MATCH (a:Memory)-[r]->(b:Memory) 
         WHERE (a.namespace = 'global' OR a.namespace = '{ns}') 
           AND (b.namespace = 'global' OR b.namespace = '{ns}')
         RETURN a.id, b.id, label(r)",
         ns = namespace_filter.replace("'", "''")
    );

    if let Ok(mut result_edges) = conn.query(&query_edges) {
        while let Some(row) = result_edges.next() {
            let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
            let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
            let rel_type = if let lbug::Value::String(s) = &row[2] { s.clone() } else { continue };
            
            links.push(serde_json::json!({
                "source": source,
                "target": target,
                "type": rel_type
            }));
        }
    }

    Ok(GraphData { nodes, links })
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            save_project_path, 
            get_graph, 
            ingest_ast, 
            get_last_project_path,
            delete_memory,
            edit_memory
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            let open_project =
                MenuItemBuilder::with_id("open_project", "Open Project").build(app)?;
            let file_menu = SubmenuBuilder::new(app, "File")
                .item(&open_project)
                .build()?;

            let menu = MenuBuilder::new(app).item(&file_menu).build()?;

            app.set_menu(menu)?;

            app.on_menu_event(move |app, event| {
                println!("Menu event triggered: {:?}", event.id());
                if event.id() == "open_project" {
                    println!("Emitting open-project-dialog event");
                    let _ = app.emit("open-project-dialog", ());
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
