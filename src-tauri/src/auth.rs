//! dsh ≥ 0.1.2-rc.1 浏览器 token 认证适配。
//!
//! dsh 每进程随机 launch token：`GET /?token=…` 换绑定 authority 的签名 cookie
//! （`SameSite=Strict`，HttpOnly，持久）。壳的 iframe 相对顶层 `tauri.localhost`
//! 是跨站，Strict cookie 不会在 iframe 请求中发送 → 需在 Rust 侧完成交换，
//! 再经 WebView2 CookieManager 注入 **SameSite=None** 的等价 cookie
//! （WebView2 默认允许第三方 cookie，None 使 iframe 内 RPC 请求可携带）。
//! 注入 cookie 持久化（Expires = unix 秒），dsh 重启/壳重启后仍有效
//! （签名 secret 持久于凭据、cookie 不绑定进程 token）。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 注入 cookie 的持久时长（秒）。dsh 侧 cookie 为天级 Max-Age，这里取 30 天。
const COOKIE_TTL_SECS: f64 = 30.0 * 24.0 * 3600.0;

/// 从 `dsh web: http://host:port/?token=…` 行提取出的完整 URL 中
/// 解析 (host, host:port, path_and_query)：host 用于 cookie domain，port 用于连接。
/// 不含 `token=` 的 URL（旧版 dsh）返回 None——无需注入。
fn parse_token_url(url: &str) -> Option<(String, String, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = rest.split_once('/')?;
    if !url.contains("token=") {
        return None; // 旧版 dsh（无 token）无需注入
    }
    let host = authority
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(authority)
        .to_string();
    Some((host, authority.to_string(), format!("/{path}")))
}

/// 对带 token 的 URL 发一次 HTTP GET（模拟浏览器根交换），
/// 返回 (cookie 名, cookie 值, host-only domain)。
pub fn exchange_cookie(url: &str) -> Option<(String, String, String)> {
    let (host, authority, path) = parse_token_url(url)?;
    let addr = format!("127.0.0.1:{port}", port = authority.rsplit_once(':')?.1);
    let mut sock = TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(800)).ok()?;
    let _ = sock.set_read_timeout(Some(Duration::from_millis(800)));
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    );
    sock.write_all(req.as_bytes()).ok()?;
    // 只读响应头（到空行）
    let mut buf = [0u8; 4096];
    let mut headers = String::new();
    loop {
        let n = sock.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        headers.push_str(&String::from_utf8_lossy(&buf[..n]));
        if headers.contains("\r\n\r\n") {
            break;
        }
        if headers.len() > 8192 {
            return None;
        }
    }
    // 找 set-cookie 头，取第一个 cookie 段（name=value）
    for line in headers.split("\r\n") {
        let lower = line.to_lowercase();
        if lower.starts_with("set-cookie:") {
            let value = line["set-cookie:".len()..].trim();
            if let Some(eq) = value.find('=') {
                let name = value[..eq].trim();
                let cookie_value = value[eq + 1..]
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim();
                if !name.is_empty() && !cookie_value.is_empty() {
                    return Some((name.to_string(), cookie_value.to_string(), host));
                }
            }
        }
    }
    None
}

/// 把 (name, value) cookie 注入 WebView2（SameSite=None + HttpOnly + Path=/ + 持久）。
/// domain 为 host-only cookie 的域名（如 127.0.0.1）。
/// 完成（或失败）后调用 `done`（注入为异步平台回调，需借此通知前端刷新）。
pub fn inject(
    win: &tauri::WebviewWindow,
    name: String,
    value: String,
    domain: String,
    done: std::sync::Arc<dyn Fn(bool) + Send + Sync>,
) {
    use tauri::webview::PlatformWebview;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE, ICoreWebView2_4,
    };
    use windows_core::Interface;
    let _ = win.as_ref().with_webview(move |platform: PlatformWebview| {
        let controller = platform.controller();
        // 安全：controller 由 tauri/wry 持有，生命周期内有效；接口引用计数由 windows-core 管理
        let Ok(core) = (unsafe { controller.CoreWebView2() }) else {
            done(false);
            return;
        };
        let _ = &core;
        let Ok(wv4) = core.cast::<ICoreWebView2_4>() else {
            done(false);
            return;
        };
        // 安全：wv4 句柄生命周期与 webview 一致
        let Ok(cookie_mgr) = (unsafe { wv4.CookieManager() }) else {
            done(false);
            return;
        };
        // 安全：cookie_mgr 引用计数受控于 COM
        let Ok(cookie) = (unsafe {
            cookie_mgr.CreateCookie(
                &windows_core::HSTRING::from(name.as_str()),
                &windows_core::HSTRING::from(value.as_str()),
                &windows_core::HSTRING::from(domain.as_str()),
                &windows_core::HSTRING::from("/"),
            )
        }) else {
            done(false);
            return;
        };
        // 安全：cookie 对象仅在本闭包内使用
        unsafe {
            let _ = cookie.SetIsHttpOnly(true);
            // SameSite=None 必须带 Secure 才会被 Chromium 接受；
            // 127.0.0.1 是 trustworthy origin，Secure cookie 在 http:// 下可用
            let _ = cookie.SetSameSite(COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE);
            let _ = cookie.SetIsSecure(true);
            // 持久化：Expires = unix 秒；此处按官方语义设 now + TTL
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let _ = cookie.SetExpires(now + COOKIE_TTL_SECS);
            if let Err(e) = cookie_mgr.AddOrUpdateCookie(&cookie) {
                eprintln!("[dsh-ui] AddOrUpdateCookie failed: {e}");
                done(false);
                return;
            }
        }
        done(true);
    });
}
