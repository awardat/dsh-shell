mod auth;
mod commands;
mod download;
mod job;
mod server;
mod settings;
mod terminal;
mod zoom;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{webview::WebviewWindowBuilder, Manager, WebviewUrl, WindowEvent};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 重复启动时唤起已有实例
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::terminal_input,
            commands::terminal_resize,
            commands::get_terminal_buffer,
            commands::zoom_step,
            commands::zoom_set,
            commands::restart_service,
            commands::stop_service,
            commands::get_settings,
            commands::save_settings,
            commands::open_browser,
        ])
        .setup(|app| {
            let app_handle = app.handle().clone();
            // 便携模式（程序目录有 portable.marker）：配置存程序目录，随目录走
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()));
            let portable = exe_dir
                .as_ref()
                .map(|d| d.join("portable.marker").exists())
                .unwrap_or(false);
            let config_dir = if portable {
                exe_dir.clone().unwrap_or_else(|| PathBuf::from("C:\\"))
            } else {
                app.path().app_config_dir()?
            };
            std::fs::create_dir_all(&config_dir)?;
            let settings = settings::Settings::load(&config_dir);
            if portable {
                eprintln!("[dsh-ui] portable mode, config dir: {}", config_dir.display());
            }

            // 全局状态必须先 manage：窗口构建后页面可能立即触发加载
            let inner = Arc::new(Mutex::new(server::AppInner::new(settings.clone(), config_dir)));
            app.manage(inner.clone());

            // 主窗口：shell 页面（iframe 内嵌 DSH UI + 右下角终端按钮/面板）
            let nav_handle = app_handle.clone();
            let mut main_builder = WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::App("index.html".into()),
            )
            .title("DeepSeek Harness")
            .inner_size(1280.0, 738.0)
            .min_inner_size(720.0, 540.0)
            // 窗口底色与启动画面一致（深色）：页面首帧渲染前不显示白屏
            .background_color(tauri::window::Color(13, 17, 23, 255));
            if std::env::var("DSH_DEBUG_DEVTOOLS").is_ok() {
                // WebView2 环境参数以首个 webview 为准
                main_builder = main_builder.additional_browser_args("--remote-debugging-port=9222");
            }
            let main = main_builder
            .on_navigation(move |url| {
                let host = url.host_str().unwrap_or("").to_string();
                let scheme = url.scheme().to_string();
                // 放行：Tauri 本地资产协议（tauri://、http://tauri.localhost）、
                // 本机 DSH 服务；其余（外链）交给系统浏览器。
                // data:/about: 为 WebView 内部导航兜底（低风险，保留）
                let local = scheme == "tauri"
                    || scheme == "data"
                    || scheme == "about"
                    || host == "tauri.localhost"
                    || (scheme == "http" && (host == "127.0.0.1" || host == "localhost"));
                if local {
                    return true;
                }
                let _ = tauri_plugin_opener::OpenerExt::opener(&nav_handle)
                    .open_url(url.as_str(), None::<&str>);
                false
            })
            .build()?;

            // 调试：DSH_DEBUG_DEVTOOLS=1 时打开 DevTools（供协议检查）
            if std::env::var("DSH_DEBUG_DEVTOOLS").is_ok() {
                let _ = main.open_devtools();
            }

            // WebView2 下载支持（session log 导出等）
            download::setup(&app_handle, &main);

            // 恢复缩放
            let zoom0 = inner.lock().unwrap().settings.zoom;
            let _ = zoom::apply(&app_handle, zoom0);

            // 启动流程（后台线程执行）：dsh 检测（where + 缓存扫描）、端口探测、cmd 会话
            // 等同步工作在后台进行，不阻塞页面首帧渲染（避免启动白屏卡顿）
            if settings.auto_start {
                let app2 = app_handle.clone();
                let inner2 = inner.clone();
                std::thread::spawn(move || server::boot(&app2, &inner2));
            } else {
                server::set_phase(
                    &app_handle,
                    &inner,
                    server::Phase::Boot,
                    Some("自动启动已关闭，请在终端中手动运行命令".into()),
                );
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                match event {
                    WindowEvent::CloseRequested { .. } => {
                        let app = window.app_handle().clone();
                        server::handle_close(&app);
                        // 主窗口关闭即退出应用（JobObject 自动清理进程树；
                        // keep-alive 模式下脱离进程已另行启动）
                        app.exit(0);
                    }
                    _ => {}
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
