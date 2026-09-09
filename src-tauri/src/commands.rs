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
pub fn terminal_input(state: Inner<'_>, data: String) -> Result<(), String> {
    let mut g = state.lock().unwrap();
    let t = g
        .term
        .as_mut()
        .ok_or_else(|| "终端尚未就绪（会话已停止或未启动）".to_string())?;
    t.write(data.as_bytes())
        .map_err(|e| format!("终端写入失败：{e}"))
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
pub fn get_settings(state: Inner<'_>) -> Result<serde_json::Value, String> {
    let g = state.lock().unwrap();
    let mut s = g.settings.clone();
    s.zoom = g.zoom;
    serde_json::to_value(crate::settings::SettingsIpc::from(s))
        .map_err(|e| format!("序列化设置失败：{e}"))
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: Inner<'_>,
    s: crate::settings::SettingsIpc,
) -> Result<(), String> {
    use crate::settings::Settings;
    // 1) 锁内：应用新值（保留 zoom/auto_start/terminal_height_ratio），
    //    序列化与取旧值也在锁内完成（纯内存操作，快）
    let (dir, text, old) = {
        let mut g = state.lock().unwrap();
        let old = g.settings.clone();
        let zoom = g.zoom;
        // auto_start / terminal_height_ratio 前端未暴露开关：保留磁盘原值，
        // 避免设置页保存时静默覆盖用户手动修改的配置
        let auto_start = g.settings.auto_start;
        let terminal_height_ratio = g.settings.terminal_height_ratio;
        let mut next = Settings::from(s).normalize();
        next.zoom = zoom;
        next.auto_start = auto_start;
        next.terminal_height_ratio = terminal_height_ratio;
        g.settings = next;
        g.url = format!("http://127.0.0.1:{}/", g.settings.port);
        let dir = g.config_dir.clone();
        let text = serde_json::to_string_pretty(&g.settings)
            .map_err(|e| format!("序列化设置失败：{e}"))?;
        (dir, text, old)
    };
    // 2) 锁外写盘（原子替换；失败回滚内存态并上报，避免 UI 报成功而磁盘旧值）
    if let Err(e) = crate::settings::write_settings(&dir, &text) {
        let mut g = state.lock().unwrap();
        g.settings = old;
        g.url = format!("http://127.0.0.1:{}/", g.settings.port);
        return Err(format!("保存设置失败：{e}"));
    }
    let _ = &app; // app 保留（参数契约）
    server::emit_state(&app, &state.inner());
    Ok(())
}

#[tauri::command]
pub fn open_browser(app: AppHandle, url: String) -> Result<(), String> {
    // 只放行 http/https（拒绝 file:/ms-settings:/自定义协议等任意 scheme）
    let lower = url.to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("仅支持 http/https 链接".into());
    }
    tauri_plugin_opener::OpenerExt::opener(&app)
        .open_url(&url, None::<&str>)
        .map_err(|e| e.to_string())
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

