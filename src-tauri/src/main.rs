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
    path.push("config.json");
    path
}

fn load_config() -> Config {
    if let Ok(content) = fs::read_to_string(get_config_path()) {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Config::default()
    }
}

fn save_config(config: &Config) {
    if let Ok(content) = serde_json::to_string_pretty(config) {
        let _ = fs::write(get_config_path(), content);
    }
}

#[tauri::command]
fn save_project_path(path: String) {
    let mut config = load_config();
    config.last_project_path = Some(path);
    save_config(&config);
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![save_project_path])
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
                if event.id() == "open_project" {
                    let _ = app.emit("open-project-dialog", ());
                }
            });

            // Load last project path
            let config = load_config();
            if let Some(path) = config.last_project_path {
                let h = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    #[derive(Clone, serde::Serialize)]
                    struct Payload {
                        path: String,
                    }
                    let _ = h.emit("load-project-path", Payload { path });
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
