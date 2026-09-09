//! 缩放：WebView2 zoom factor（浏览器同款渲染级缩放），50%–300%，持久化。
//! 前端（shell 页面）通过 zoom_step / zoom_set 命令控制。

use crate::server::{self, AppInner};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

pub const MIN_ZOOM: f64 = 0.5;
pub const MAX_ZOOM: f64 = 3.0;
const STEP: f64 = 0.1;

fn clamp(f: f64) -> f64 {
    // 拒绝非有限值（NaN/Inf），防污染持久化与后续恢复
    if !f.is_finite() {
        return 1.0;
    }
    f.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// 应用缩放因子并持久化。
pub fn apply(app: &AppHandle, factor: f64) -> Result<f64, String> {
    let factor = clamp(factor);
    let Some(main) = app.get_webview_window("main") else {
        return Err("主窗口不存在".into());
    };
    main.set_zoom(factor).map_err(|e| e.to_string())?;
    let inner = app.state::<Arc<Mutex<AppInner>>>();
    let (dir, text, old_zoom, old_settings) = {
        let mut g = inner.lock().unwrap();
        let old_zoom = g.zoom;
        let old_settings = g.settings.zoom;
        g.zoom = factor;
        g.settings.zoom = factor;
        (
            g.config_dir.clone(),
            serde_json::to_string_pretty(&g.settings).unwrap_or_default(),
            old_zoom,
            old_settings,
        )
    };
    if let Err(e) = crate::settings::write_settings(&dir, &text) {
        // 回滚状态 + 窗口 zoom
        {
            let mut g = inner.lock().unwrap();
            g.zoom = old_zoom;
            g.settings.zoom = old_settings;
        }
        let _ = main.set_zoom(old_zoom);
        return Err(format!("保存缩放设置失败：{e}"));
    }
    server::emit_state(app, &inner);
    Ok(factor)
}

pub fn step(app: &AppHandle, delta: i8) -> Result<f64, String> {
    // 单锁内读-算-写（原子），避免并发 step 丢失增量
    let inner = app.state::<Arc<Mutex<AppInner>>>();
    let Some(main) = app.get_webview_window("main") else {
        return Err("主窗口不存在".into());
    };
    let (factor, old_zoom, old_settings, dir, text) = {
        let mut g = inner.lock().unwrap();
        let f = clamp(g.zoom + f64::from(delta) * STEP);
        let old_zoom = g.zoom;
        let old_settings = g.settings.zoom;
        g.zoom = f;
        g.settings.zoom = f;
        (
            f,
            old_zoom,
            old_settings,
            g.config_dir.clone(),
            serde_json::to_string_pretty(&g.settings).unwrap_or_default(),
        )
    };
    main.set_zoom(factor).map_err(|e| e.to_string())?;
    if let Err(e) = crate::settings::write_settings(&dir, &text) {
        {
            let mut g = inner.lock().unwrap();
            g.zoom = old_zoom;
            g.settings.zoom = old_settings;
        }
        let _ = main.set_zoom(old_zoom);
        return Err(format!("保存缩放设置失败：{e}"));
    }
    server::emit_state(app, &inner);
    Ok(factor)
}

pub fn set(app: &AppHandle, factor: f64) -> Result<f64, String> {
    apply(app, factor)
}
