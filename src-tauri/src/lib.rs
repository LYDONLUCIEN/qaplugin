// QA Snapshot — entry point.
// Boots the desktop client, registers global hotkeys, and connects to
// the cloud command/answer service. Screenshots always happen locally.

#![allow(clippy::needless_return)]

mod cloud;
mod hub;
mod platform;
mod screenshot;

use std::time::Duration;

use tauri::{Manager, WindowEvent};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

#[derive(Clone)]
pub struct AppState {
    pub app: tauri::AppHandle,
}

#[tauri::command]
async fn trigger_capture(
    question: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let app = state.app.clone();
    screenshot::capture_and_ask(app, question, None, None).await
}

#[tauri::command]
async fn set_overlay_protected(protected: bool, app: tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("overlay") {
        platform::set_window_protected(&w, protected).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn toggle_overlay_visible(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("overlay") {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
        } else {
            let _ = w.show();
        }
    }
    Ok(())
}

#[tauri::command]
async fn save_cloud_config(
    cloud_url: String,
    web_url: String,
    device_id: String,
    device_token: Option<String>,
    model_profile: Option<String>,
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let config = cloud::save(
        &app,
        cloud_url,
        web_url,
        device_id,
        device_token,
        model_profile,
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "cloudUrl": config.cloud_url,
        "webUrl": config.web_url,
        "phoneUrl": config.phone_url(),
        "deviceId": config.device_id,
        "modelProfile": config.model_profile,
    }))
}

#[tauri::command]
async fn test_cloud_connection(
    cloud_url: String,
    device_id: String,
    device_token: Option<String>,
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let device_token = device_token
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            cloud::current(&app)
                .filter(|config| config.device_id == device_id.trim())
                .map(|config| config.device_token)
        })
        .ok_or_else(|| "请输入设备 Token 后再测试".to_string())?;
    let result = cloud::test_connection(cloud_url, device_id, device_token)
        .await
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "defaultModelProfile": result.default_model_profile,
        "profiles": result.profiles,
    }))
}

#[tauri::command]
async fn test_model_profile(
    cloud_url: String,
    device_id: String,
    device_token: Option<String>,
    model_profile: String,
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let device_token = device_token
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            cloud::current(&app)
                .filter(|config| config.device_id == device_id.trim())
                .map(|config| config.device_token)
        })
        .ok_or_else(|| "请输入设备 Token 后再测试".to_string())?;
    let result = cloud::test_model_profile(cloud_url, device_id, device_token, model_profile)
        .await
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "profileId": result.profile_id,
        "sample": result.sample,
    }))
}

#[tauri::command]
async fn get_status(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let overlay_visible = app
        .get_webview_window("overlay")
        .map(|window| window.is_visible().unwrap_or(false))
        .unwrap_or(false);
    match cloud::current(&app) {
        Some(config) => Ok(serde_json::json!({
            "cloudUrl": config.cloud_url,
            "webUrl": config.web_url,
            "phoneUrl": config.phone_url(),
            "deviceId": config.device_id,
            "modelProfile": config.model_profile,
            "cloudConnected": cloud::connected(),
            "overlayVisible": overlay_visible,
            "configError": null,
        })),
        None => Ok(serde_json::json!({
            "cloudUrl": "",
            "webUrl": "",
            "phoneUrl": "",
            "deviceId": "",
            "cloudConnected": false,
            "overlayVisible": overlay_visible,
            "configError": "尚未配置云端服务",
        })),
    }
}

pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    let shortcut = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space);

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = screenshot::capture_and_ask(app, None, None, None).await;
                        });
                    }
                })
                .build(),
        )
        .setup(move |app| {
            let state = AppState {
                app: app.handle().clone(),
            };
            app.manage(state);

            hub::init(app.handle());
            cloud::init(app.handle());
            cloud::start_command_loop(app.handle().clone());

            // Apply anti-capture protection to the overlay window.
            if let Some(overlay) = app.get_webview_window("overlay") {
                if let Err(e) = platform::set_window_protected(&overlay, true) {
                    log::warn!("anti-capture init failed: {e}");
                }
                // Watcher thread: re-checks protection state periodically.
                let ov = overlay.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        let _ = platform::refresh(&ov);
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                });
            }

            // Register the global hotkey (handler was set on the plugin).
            if let Err(e) = app.global_shortcut().register(shortcut) {
                log::warn!("failed to register global shortcut: {e}");
            } else {
                log::info!("Global hotkey Ctrl+Shift+Space registered.");
            }

            log::info!("QA Snapshot desktop ready. Press Ctrl+Shift+Space to capture.");

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "control" {
                    // Hide instead of quitting — keep the daemon alive.
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            trigger_capture,
            set_overlay_protected,
            toggle_overlay_visible,
            save_cloud_config,
            test_cloud_connection,
            test_model_profile,
            get_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
