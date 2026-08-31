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

/// A client for talking to the daemon, with the timeout stated rather than
/// inherited.
///
/// `reqwest::blocking::Client::new()` applies a THIRTY SECOND timeout of its
/// own -- the blocking client does this, the async client does not -- and every
/// call here used to take it. A directory ingest of a real repository runs
/// longer than that, so the GUI was aborting its own ingest partway through:
/// the daemon saw the client disconnect, dropped the request with it, and
/// stopped mid-walk without ever reaching relink_edges. What the user saw was a
/// graph frozen half-built with no GOVERNS edges, a daemon still answering
/// /health, and no error anywhere (bead neurostrata-zbw).
fn daemon_client(timeout: std::time::Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        // Separate from the call's own bound: a port with nothing behind it
        // hangs rather than refusing here, so without this a daemon that is not
        // running is indistinguishable from one that is merely busy.
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

/// Ingestion walks and embeds a whole repository, which is minutes of work on a
/// large one.
const INGEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

/// Everything else is a single query, but not necessarily a fast one: /graph
/// has been measured at 28s while a write was in flight, so the old 30s left
/// nothing spare.
const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

fn ensure_daemon() -> Result<(), String> {
    // Check if daemon is responding
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .map_err(|e| e.to_string())?;

    if client.get("http://127.0.0.1:34343/health").send().is_err() {
        // Not running, spawn it
        println!("MCP Daemon not running. Starting it...");

        let mut tried: Vec<String> = Vec::new();
        let mut spawned = false;

        for candidate in daemon_candidates() {
            tried.push(candidate.display().to_string());
            match std::process::Command::new(&candidate).arg("daemon").spawn() {
                Ok(_) => {
                    spawned = true;
                    break;
                }
                // Only "not found" is worth trying the next candidate for.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("Failed to spawn {}: {}", candidate.display(), e)),
            }
        }

        if !spawned {
            return Err(format!(
                "Could not find the neurostrata-mcp binary. Tried: {}. Install it (cargo install --path .) or put it on PATH.",
                tried.join(", ")
            ));
        }

        // Wait for it to boot
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Ok(())
}

/// Where to look for the daemon binary, in order.
///
/// The bare command comes first because that is what every MCP registration
/// this project writes uses, and it is the only form that finds the `.exe` on
/// Windows. The hard-coded `~/.local/bin/neurostrata-mcp` that used to be the
/// only candidate cannot exist there at all, so the GUI could never autostart
/// the daemon on Windows (bead neurostrata-a9a) -- and `cargo install` puts it
/// in `~/.cargo/bin` on every platform anyway.
fn daemon_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("neurostrata-mcp")];

    if let Some(home) = dirs::home_dir() {
        for relative in [".cargo/bin/neurostrata-mcp", ".local/bin/neurostrata-mcp"] {
            candidates.push(home.join(relative));
        }
    }

    candidates
}

/// The namespace a project's memories live under.
///
/// Nothing in the design derives this from a directory: the MCP schema calls it
/// "the exact project name". Deriving it from the checkout folder was this
/// application's own invention, and it split one project into two strata when
/// the folder was cloned as `neurostrata` while the project is `NeuroStrata`
/// (bead neurostrata-fld).
///
/// So it is decided once and remembered. The folder name only seeds the first
/// answer; after that the stored name is what this project is called, whatever
/// the directory is renamed to afterwards. The daemon still resolves case, so an
/// older seed keeps finding the namespace it named.
fn namespace_for(project_path: &str) -> String {
    if let Ok(stored) = fs::read_to_string(namespace_path(project_path)) {
        let stored = stored.trim().to_string();
        if !stored.is_empty() {
            return stored;
        }
    }

    let seeded = std::path::Path::new(project_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("global")
        .to_string();

    let record = namespace_path(project_path);
    if let Some(parent) = record.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&record, &seeded).ok();
    seeded
}

/// Kept beside the project, in the directory add_memory already uses as the
/// marker that a project has a stratum at all.
fn namespace_path(project_path: &str) -> PathBuf {
    std::path::Path::new(project_path)
        .join(".NeuroStrata")
        .join("namespace")
}

/// Hands a URL to the OS to open, and says so when it cannot.
///
/// The frontend used tauri-plugin-shell's `open` for this. On Windows nothing
/// happened and nothing was reported -- the call was rejected before it reached
/// the shell, and the only trace was a console.error the release build does not
/// surface. Meanwhile the identical URL opens correctly through ShellExecute,
/// which is what this does.
///
/// The result is a Result on purpose: a button that silently does nothing is
/// indistinguishable from a missing editor, a bad path, or a broken build, and
/// this one was mistaken for all three.
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    // Only schemes the visualizer actually emits. An unrestricted opener
    // reachable from the webview would run whatever a memory's content asked
    // for, and node content is not ours.
    const ALLOWED: [&str; 4] = ["vscode://", "cursor://", "https://", "http://"];
    if !ALLOWED.iter().any(|s| url.starts_with(s)) {
        return Err(format!(
            "Refusing to open '{}': only {} are allowed.",
            url,
            ALLOWED.join(", ")
        ));
    }

    // The `open` crate rather than a hand-rolled Command per platform: it is
    // what tauri-plugin-shell uses underneath, so it is already in the tree, and
    // it knows the cases that are easy to get subtly wrong -- `start` being a
    // cmd builtin whose first quoted argument is the window title, xdg-open
    // versus the desktop-specific openers, quoting on each.
    open::that(&url).map_err(|e| format!("Could not open '{}': {}", url, e))
}

#[tauri::command]
fn ingest_ast(project_path: String) -> Result<String, String> {
    ensure_daemon()?;
    
    let namespace = namespace_for(&project_path);

    let client = daemon_client(INGEST_TIMEOUT)?;
    let resp = client.post("http://127.0.0.1:34343/ingest")
        .json(&serde_json::json!({
            "dir": project_path,
            "namespace": namespace
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
    
    let client = daemon_client(QUERY_TIMEOUT)?;
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

    let client = daemon_client(QUERY_TIMEOUT)?;
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
        namespace_filter = namespace_for(path_str);
    }
    
    let client = daemon_client(QUERY_TIMEOUT)?;
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
            open_external,
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