//! IPC 命令层：前端（启动页 / 浮层 / DSH 页面注入脚本）→ Rust。

use crate::server::{self, AppInner, Phase};
use crate::zoom;
use std::sync::{Arc, Mutex};
use tauri::AppHandle;

type Inner<'a> = tauri::State<'a, Arc<Mutex<AppInner>>>;

#[tauri::command]
pub fn get_state(_app: AppHandle, state: Inner<'_>) -> serde_json::Value {
    let g = state.lock().unwrap();
    serde_json::json!({
        "phase": g.phase,
        "message": g.message,
        "url": g.url,
        "zoom": g.zoom,
    })
}

#[tauri::command]
pub fn terminal_input(state: Inner<'_>, data: String) {
    let mut g = state.lock().unwrap();
    if let Some(t) = g.term.as_mut() {
        t.write(data.as_bytes());
    }
}

#[tauri::command]
pub fn get_terminal_buffer(state: Inner<'_>) -> String {
    state.lock().unwrap().term_buffer.clone()
}

#[tauri::command]
pub fn terminal_resize(state: Inner<'_>, cols: u16, rows: u16) {
    let mut g = state.lock().unwrap();
    if let Some(t) = g.term.as_mut() {
        t.resize(rows, cols);
    }
}

#[tauri::command]
pub fn zoom_step(app: AppHandle, delta: i8) -> Result<f64, String> {
    zoom::step(&app, delta)
}

#[tauri::command]
pub fn zoom_set(app: AppHandle, factor: f64) -> Result<f64, String> {
    zoom::set(&app, factor)
}

#[tauri::command]
pub fn restart_service(app: AppHandle, state: Inner<'_>) {
    server::restart(&app, &state.inner());
}

#[tauri::command]
pub fn stop_service(app: AppHandle, state: Inner<'_>) -> Result<(), String> {
    server::stop(&app, &state.inner())
}

#[tauri::command]
pub fn get_settings(state: Inner<'_>) -> serde_json::Value {
    let g = state.lock().unwrap();
    let mut s = g.settings.clone();
    s.zoom = g.zoom;
    serde_json::to_value(crate::settings::SettingsIpc::from(s)).unwrap_or_default()
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: Inner<'_>,
    s: crate::settings::SettingsIpc,
) -> Result<(), String> {
    {
        let mut g = state.lock().unwrap();
        // zoom 由缩放操作管理，不随设置页保存（前端传的 zoom 仅作占位）
        let zoom = g.zoom;
        // auto_start / terminal_height_ratio 前端未暴露开关：保留磁盘原值，
        // 避免设置页保存时静默覆盖用户手动修改的配置
        let auto_start = g.settings.auto_start;
        let terminal_height_ratio = g.settings.terminal_height_ratio;
        g.settings = crate::settings::Settings::from(s);
        g.settings.zoom = zoom;
        g.settings.auto_start = auto_start;
        g.settings.terminal_height_ratio = terminal_height_ratio;
        g.url = format!("http://127.0.0.1:{}/", g.settings.port);
        let dir = g.config_dir.clone();
        if let Err(e) = g.settings.save(&dir) {
            eprintln!("[dsh-ui] {e}");
        }
        let _ = &app; // app 保留（参数契约）
    }
    server::emit_state(&app, &state.inner());
    Ok(())
}

#[tauri::command]
pub fn open_browser(app: AppHandle, url: String) {
    let _ = tauri_plugin_opener::OpenerExt::opener(&app).open_url(&url, None::<&str>);
}

/// 供 Rust 内部使用的兜底（当前未用）
#[allow(dead_code)]
pub fn _phase_label(p: Phase) -> &'static str {
    match p {
        Phase::Boot => "boot",
        Phase::Ready => "ready",
        Phase::Failed => "failed",
        Phase::Stopped => "stopped",
    }
}

