//! 缩放：WebView2 zoom factor（浏览器同款渲染级缩放），50%–300%，持久化。
//! 前端（shell 页面）通过 zoom_step / zoom_set 命令控制。

use crate::server::{self, AppInner};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

pub const MIN_ZOOM: f64 = 0.5;
pub const MAX_ZOOM: f64 = 3.0;
const STEP: f64 = 0.1;

fn clamp(f: f64) -> f64 {
    f.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// 应用缩放因子并持久化。
pub fn apply(app: &AppHandle, factor: f64) -> Result<f64, String> {
    let factor = clamp(factor);
    let Some(main) = app.get_webview_window("main") else {
        return Err("主窗口不存在".into());
    };
    main.set_zoom(factor).map_err(|e| e.to_string())?;

    {
        let inner = app.state::<Arc<Mutex<AppInner>>>();
        let mut g = inner.lock().unwrap();
        g.zoom = factor;
        g.settings.zoom = factor;
        let dir = g.config_dir.clone();
        g.settings.save(&dir);
    }
    server::emit_state(app, &app.state::<Arc<Mutex<AppInner>>>());
    Ok(factor)
}

pub fn step(app: &AppHandle, delta: i8) -> Result<f64, String> {
    let current = app
        .state::<Arc<Mutex<AppInner>>>()
        .lock()
        .unwrap()
        .zoom;
    apply(app, current + f64::from(delta) * STEP)
}

pub fn set(app: &AppHandle, factor: f64) -> Result<f64, String> {
    apply(app, factor)
}
