//! WebView2 下载支持：处理 DownloadStarting（设置保存路径）与完成通知。
//!
//! 背景：wry 默认下载 handler 只放行不设路径，WebView2 无内置"另存为"UI，
//! ResultFilePath 为空时下载被取消（表现：点击导出无反应）。
//! 这里在主窗口 webview 上挂标准 WebView2 下载事件：
//! 下载 → 存到 %USERPROFILE%\Downloads + 服务端建议文件名 → 完成/失败事件通知前端。

use std::path::PathBuf;
use tauri::{AppHandle, Emitter, WebviewWindow};
use webview2_com::{
    DownloadStartingEventHandler, PermissionRequestedEventHandler, StateChangedEventHandler,
    Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_4, ICoreWebView2_8, COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED,
        COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS,
        COREWEBVIEW2_PERMISSION_KIND_MULTIPLE_AUTOMATIC_DOWNLOADS,
        COREWEBVIEW2_PERMISSION_STATE_ALLOW,
    },
};
use windows_core::{Interface, PWSTR};

/// 默认下载目录：%USERPROFILE%\Downloads（不存在则退回用户主目录；
/// 连主目录都拿不到时用系统临时目录——盘符根对标准用户不可写）
fn download_dir() -> PathBuf {
    if let Ok(home) = std::env::var("USERPROFILE") {
        let dl = PathBuf::from(&home).join("Downloads");
        if dl.is_dir() {
            return dl;
        }
        return PathBuf::from(home);
    }
    std::env::temp_dir()
}

fn pwstr_to_string(pw: PWSTR) -> String {
    // 安全：WebView2 回调传出的 PWSTR 指向事件参数内部缓冲区，事件参数在回调
    // 返回前保持有效；to_string 复制内容，不保留指针。
    unsafe { pw.to_string() }.unwrap_or_default()
}

/// 从 Content-Disposition 头解析文件名（纯函数，可单测）。
/// 支持 `filename="x"`、无引号 `filename=x`、RFC 5987 `filename*=UTF-8''x`。
fn parse_disposition_filename(cd: &str) -> Option<String> {
    // filename*=UTF-8''<pct-encoded>
    if let Some(i) = cd.find("filename*=") {
        let rest = cd[i + "filename*=".len()..].trim();
        let value = rest.split(';').next().unwrap_or("");
        let encoded = value.split("''").nth(1).unwrap_or("");
        if !encoded.is_empty() {
            let decoded = percent_decode(encoded);
            if !decoded.is_empty() {
                return Some(decoded);
            }
        }
    }
    // filename="x" 或 filename=x
    if let Some(i) = cd.find("filename=") {
        let rest = cd[i + "filename=".len()..].trim();
        if let Some(stripped) = rest.strip_prefix('"') {
            let name = stripped.split('"').next().unwrap_or("").trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        } else {
            let name = rest.split(';').next().unwrap_or("").trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// 极简百分号解码（RFC 5987 文件名），非 %XX 字符原样保留。
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Windows 保留设备名（大小写不敏感；含扩展名仍保留，如 CON.zip）
fn is_reserved_device_name(stem: &str) -> bool {
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "COM2" | "COM3" | "COM4" | "COM5"
            | "COM6" | "COM7" | "COM8" | "COM9" | "LPT1" | "LPT2" | "LPT3" | "LPT4"
            | "LPT5" | "LPT6" | "LPT7" | "LPT8" | "LPT9"
    )
}

/// 清洗下载文件名：拒绝空 / `.` / `..` / 含盘符的值；含路径分隔符（`/` `\`）时
/// 取最后一段（剥离绝对路径与穿越目录）；再过滤 Windows 非法字符
/// （`< > " | ? *`、控制字符 0x00–0x1F、尾部点/空格）与保留设备名；
/// 仍非法则返回 None（调用方走下一级兜底）。
fn sanitize_filename(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    let seg = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    if seg.is_empty() || seg == "." || seg == ".." || seg.contains(':') {
        return None;
    }
    // 尾部点/空格（Windows 不允许）
    let seg = seg.trim_end_matches(['.', ' ']);
    if seg.is_empty() {
        return None;
    }
    // 非法字符与控制字符
    if seg
        .chars()
        .any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*') || (c as u32) < 0x20)
    {
        return None;
    }
    // 保留设备名（含扩展名也保留：按主干判断）
    let stem = seg.split('.').next().unwrap_or(&seg);
    if is_reserved_device_name(stem) {
        return None;
    }
    Some(seg.to_string())
}

/// 从 Content-Disposition + URL 解析下载文件名；
/// 兜底从 URL 的 sessionId 生成 dsh 命名；再兜底时间戳。
fn resolve_filename(
    operation: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2DownloadOperation,
    url: &str,
) -> String {
    // 1) Content-Disposition 的 filename（清洗后仍非法则落入下一级）
    let mut pw = PWSTR::null();
    if unsafe { operation.ContentDisposition(&mut pw) }.is_ok() {
        let cd = pwstr_to_string(pw);
        if let Some(name) = parse_disposition_filename(&cd).and_then(|n| sanitize_filename(&n)) {
            return name;
        }
    }
    // 2) query 里的 sessionId（dsh 导出约定命名）
    if let Some(i) = url.find("sessionId=") {
        let rest = &url[i + "sessionId=".len()..];
        let id: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !id.is_empty() {
            return format!("dsh-session-{id}.zip");
        }
    }
    // 3) 兜底
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("download-{ts}.zip")
}

/// 挂载下载事件。token 不保存（进程生命周期内有效，无需注销）。
pub fn setup(app: &AppHandle, main: &WebviewWindow) {
    let app = app.clone();
    let _ = main.as_ref().with_webview(move |platform| {
        let controller = platform.controller();
        // 安全：controller 由 tauri/wry 持有并在 webview 生命周期内有效；
        // CoreWebView2 返回的接口引用计数由 windows-core 管理，闭包持 handle 不持指针。
        let Ok(core) = (unsafe { controller.CoreWebView2() }) else {
            eprintln!("[dsh-ui] download: no ICoreWebView2");
            return;
        };
        let Ok(wv4) = core.cast::<ICoreWebView2_4>() else {
            eprintln!("[dsh-ui] download: no ICoreWebView2_4");
            return;
        };

        // 自动允许"下载多个文件"权限（否则 WebView2 弹 edge://permission-request-dialog）
        // 安全收窄：仅放行来自本机服务（loopback）或应用自身资产的下载请求，
        // 其余来源保持默认行为（拒绝）
        if let Ok(wv8) = core.cast::<ICoreWebView2_8>() {
            let handle_perm = app.clone();
            let perm_handler = PermissionRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else {
                    return Ok(());
                };
                // 安全：事件参数指针由 WebView2 在回调期间保证有效
                let mut kind = webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_PERMISSION_KIND(
                    0,
                );
                let _ = unsafe { args.PermissionKind(&mut kind) };
                if kind == COREWEBVIEW2_PERMISSION_KIND_MULTIPLE_AUTOMATIC_DOWNLOADS {
                    // 校验请求来源：取请求 Uri 的 host（仅 http/https）
                    let mut uri_pw = PWSTR::null();
                    let _ = unsafe { args.Uri(&mut uri_pw) };
                    let uri = pwstr_to_string(uri_pw);
                    let host = uri
                        .strip_prefix("http://")
                        .or_else(|| uri.strip_prefix("https://"))
                        .and_then(|r| r.split('/').next())
                        .and_then(|a| a.split(':').next());
                    let trusted =
                        matches!(host, Some("127.0.0.1" | "localhost" | "tauri.localhost"));
                    if trusted {
                        let _ = unsafe { args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW) };
                    } else {
                        eprintln!("[dsh-ui] download permission denied for origin: {uri}");
                    }
                }
                let _ = handle_perm;
                Ok(())
            }));
            let mut token: i64 = 0;
            if unsafe { wv8.add_PermissionRequested(&perm_handler, &mut token) }.is_err() {
                eprintln!("[dsh-ui] download: add_PermissionRequested failed");
            }
        }

        // 下载起始：确定文件名/路径、挂完成通知、放行
        let handle = app.clone();
        let handler = DownloadStartingEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else {
                return Ok(());
            };
            // 安全：事件参数与下载操作对象在回调期间由 WebView2 保证有效；
            // 闭包捕获的 app handle / path 均为自有数据，不持有 COM 指针跨回调
            let operation = unsafe { args.DownloadOperation() }?;
            let uri = {
                let mut pw = PWSTR::null();
                let _ = unsafe { operation.Uri(&mut pw) };
                pwstr_to_string(pw)
            };
            let name = resolve_filename(&operation, &uri);
            let path = download_dir().join(&name);

            // 完成通知（StateChanged：离开 IN_PROGRESS 即终态）
            let handle_done = handle.clone();
            let path_done = path.clone();
            let state_changed = StateChangedEventHandler::create(Box::new(move |op, _| {
                let Some(op) = op else {
                    return Ok(());
                };
                // 安全：同上，事件参数在回调期间有效
                let mut state =
                    webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_DOWNLOAD_STATE(0);
                let _ = unsafe { op.State(&mut state) };
                if state != COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS {
                    let ok = state == COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED;
                    let _ = handle_done.emit(
                        "download:completed",
                        serde_json::json!({ "path": path_done.to_string_lossy(), "ok": ok }),
                    );
                }
                Ok(())
            }));
            let mut token: i64 = 0;
            if unsafe { operation.add_StateChanged(&state_changed, &mut token) }.is_err() {
                eprintln!("[dsh-ui] download: add_StateChanged failed");
            }

            // 设置保存路径并放行；失败也发完成事件（ok:false），避免"点击无反应"
            let hstr = windows_core::HSTRING::from(path.to_string_lossy().to_string());
            let set_path_ok = unsafe { args.SetResultFilePath(&hstr) }.is_ok();
            let handled_ok = unsafe { args.SetHandled(true) }.is_ok();
            if !set_path_ok || !handled_ok {
                eprintln!(
                    "[dsh-ui] download: set result path/handled failed (path_ok={set_path_ok}, handled_ok={handled_ok})"
                );
                let _ = handle.emit(
                    "download:completed",
                    serde_json::json!({
                        "path": path.to_string_lossy(),
                        "ok": false,
                    }),
                );
                return Ok(());
            }
            let _ = handle.emit(
                "download:starting",
                serde_json::json!({ "path": path.to_string_lossy(), "name": name }),
            );
            Ok(())
        }));
        let mut token: i64 = 0;
        if unsafe { wv4.add_DownloadStarting(&handler, &mut token) }.is_err() {
            eprintln!("[dsh-ui] download: add_DownloadStarting failed");
        } else {
            eprintln!("[dsh-ui] download handler installed");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_quoted_filename() {
        assert_eq!(
            parse_disposition_filename(r#"attachment; filename="dsh-session-a.zip""#).as_deref(),
            Some("dsh-session-a.zip")
        );
    }

    #[test]
    fn disposition_unquoted_filename() {
        assert_eq!(
            parse_disposition_filename("attachment; filename=report.zip").as_deref(),
            Some("report.zip")
        );
    }

    #[test]
    fn disposition_rfc5987_utf8_filename() {
        assert_eq!(
            parse_disposition_filename("attachment; filename*=UTF-8''%E6%B5%8B%E8%AF%95.zip")
                .as_deref(),
            Some("测试.zip")
        );
    }

    #[test]
    fn disposition_missing_returns_none() {
        assert_eq!(parse_disposition_filename("attachment"), None);
    }

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("%E6%B5%8B"), "测");
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn sanitize_filename_keeps_plain_names() {
        assert_eq!(sanitize_filename("report.zip").as_deref(), Some("report.zip"));
        assert_eq!(sanitize_filename(" 日志 导出.zip ").as_deref(), Some("日志 导出.zip"));
    }

    #[test]
    fn sanitize_filename_strips_paths_and_rejects_traversal() {
        // 绝对路径 → 取末段
        assert_eq!(sanitize_filename("/Windows/evil.exe").as_deref(), Some("evil.exe"));
        assert_eq!(sanitize_filename("C:\\Windows\\evil.exe").as_deref(), Some("evil.exe"));
        // 向上穿越 → 末段仍合法则取末段
        assert_eq!(sanitize_filename("../../Users/x/evil").as_deref(), Some("evil"));
        // 纯穿越/空/盘符 → 拒绝
        assert_eq!(sanitize_filename(".."), None);
        assert_eq!(sanitize_filename("../../.."), None);
        assert_eq!(sanitize_filename(""), None);
        assert_eq!(sanitize_filename("C:"), None);
        assert_eq!(sanitize_filename("a:b.zip"), None);
    }

    #[test]
    fn sanitize_filename_rejects_windows_invalid_names() {
        // 非法字符 / 控制字符 / 尾点空格（修剪为合法名）
        assert_eq!(sanitize_filename("report?.zip"), None);
        assert_eq!(sanitize_filename("a<b>c.zip"), None);
        assert_eq!(sanitize_filename("evil."), Some("evil".into()));
        assert_eq!(sanitize_filename("name with trailing "), Some("name with trailing".into()));
        assert_eq!(sanitize_filename("bad\u{1f}.zip"), None);
        assert_eq!(sanitize_filename("..."), None); // 全为点 → 修剪后为空
        // 保留设备名（含扩展名）
        assert_eq!(sanitize_filename("CON"), None);
        assert_eq!(sanitize_filename("con.zip"), None);
        assert_eq!(sanitize_filename("NUL"), None);
        assert_eq!(sanitize_filename("COM1.txt"), None);
        assert_eq!(sanitize_filename("LPT9"), None);
        // 合法名不受影响
        assert_eq!(sanitize_filename("console.log.txt").as_deref(), Some("console.log.txt"));
        assert_eq!(sanitize_filename("report.final.zip").as_deref(), Some("report.final.zip"));
    }
}
