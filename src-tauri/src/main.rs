// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod control;
mod display;
mod server;
mod state;
mod sync;

#[tauri::command]
fn get_state(app: tauri::AppHandle) -> serde_json::Value {
    state::state_json(&app)
}

#[tauri::command]
fn dispatch(app: tauri::AppHandle, command: state::Command) {
    state::dispatch(&app, command);
}

fn main() {
    tauri::Builder::default()
        .manage(state::Shared::new())
        .invoke_handler(tauri::generate_handler![get_state, dispatch])
        .setup(|app| {
            let handle = app.handle();
            state::set_version(&handle);
            state::load(&handle);
            state::apply_env_overrides(&handle);
            // Monitor enumeration blocks on the event loop, which hasn't
            // started yet inside setup — do startup work off-thread.
            tauri::async_runtime::spawn(async move {
                state::refresh_system(&handle);
                server::restart(handle.clone());
                control::start(handle.clone());
                sync::start(handle.clone());
                state::broadcast(&handle);
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
