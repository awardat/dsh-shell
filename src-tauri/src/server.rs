//! 生命周期状态机：cmd 会话管理、HTTP 就绪探测、浮层几何同步、退出清理。

use crate::settings::Settings;
use crate::terminal;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Boot,
    Ready,
    Failed,
    /// 用户手动停止服务后进入的状态（区别于启动失败）
    Stopped,
}

pub struct AppInner {
    pub settings: Settings,
    /// 配置目录（便携模式=程序目录，否则 %APPDATA%\com.dsh-ui.app）
    pub config_dir: PathBuf,
    pub phase: Phase,
    pub message: Option<String>,
    pub url: String,
    pub zoom: f64,
    pub term: Option<terminal::TerminalSession>,
    /// 终端输出环形缓冲：页面晚订阅时补发快照，避免启动早期输出丢失
    pub term_buffer: String,
    /// 连续自动重启计数（达上限后停止）
    pub restart_count: u32,
    /// 探针/保活线程的代次：重启服务时自增，旧线程据此退出
    pub gen: u64,
    pub probe_stop: Arc<AtomicBool>,
    /// 本次启动生效的就绪超时（秒）：首次运行需下载 dsh 时自动放宽
    pub ready_timeout: u64,
    /// 本次启动是否处于"首次运行需下载 dsh"场景（用于提示与超时文案）
    pub needs_download: bool,
    /// 新版 dsh（≥0.1.2-rc.1）：终端已捕获到带 token 的 URL（认证交换待完成）
    pub auth_pending: bool,
    /// 新版 dsh：认证 cookie 已注入 WebView2（iframe 可直接加载）
    pub auth_done: bool,
}

/// 终端输出缓冲上限（字节）
const TERM_BUFFER_MAX: usize = 65536;

/// 保留字符串**后半段**最多 `keep` 字节（原长度超过时）。
/// 按 UTF-8 字符边界截断，多字节字符不会 panic。
pub fn trim_window(s: &str, keep: usize) -> String {
    if s.len() > keep {
        let cut = s.floor_char_boundary(s.len() - keep);
        s[cut..].to_string()
    } else {
        s.to_string()
    }
}

/// 截断终端缓冲到上限（保留后半段）。
pub fn truncate_term_buffer(buf: &mut String) {
    if buf.len() > TERM_BUFFER_MAX {
        *buf = trim_window(buf, TERM_BUFFER_MAX / 2);
    }
}

impl AppInner {
    pub fn new(settings: Settings, config_dir: PathBuf) -> Self {
        let url = format!("http://127.0.0.1:{}/", settings.port);
        let zoom = settings.zoom;
        let ready_timeout = settings.ready_timeout_sec;
        Self {
            settings,
            config_dir,
            phase: Phase::Boot,
            message: None,
            url,
            zoom,
            term: None,
            term_buffer: String::new(),
            restart_count: 0,
            gen: 0,
            probe_stop: Arc::new(AtomicBool::new(false)),
            ready_timeout,
            needs_download: false,
            auth_pending: false,
            auth_done: false,
        }
    }
}

// ---------- 状态广播 ----------

pub fn emit_state(app: &AppHandle, inner: &Arc<Mutex<AppInner>>) {
    let g = inner.lock().unwrap();
    let _ = app.emit(
        "state:changed",
        serde_json::json!({
            "phase": g.phase,
            "message": g.message,
            "url": g.url,
            "zoom": g.zoom,
        }),
    );
}

pub fn set_phase(
    app: &AppHandle,
    inner: &Arc<Mutex<AppInner>>,
    phase: Phase,
    message: Option<String>,
) {
    let log_msg = message.clone().unwrap_or_default();
    {
        let mut g = inner.lock().unwrap();
        g.phase = phase;
        g.message = message;
    }
    eprintln!("[dsh-ui] phase -> {phase:?} {log_msg}");
    emit_state(app, inner);
}

// ---------- 工作目录 ----------

fn resolve_workdir(settings: &Settings) -> PathBuf {
    let wd = settings.working_dir.trim();
    if !wd.is_empty() {
        PathBuf::from(wd)
    } else {
        std::env::var("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("C:\\"))
    }
}

/// dsh 会话目录名（projectKey）解码为候选路径。
/// 编码规则（dsh-session-persistence-jsonl）：`/` `\` `:` → `-`（连续合并），
/// 安全字符保留（含 `-` 本身），其余 → `~XXXX`，外层包 `--...--`。
/// 编码有损，解码生成"分段合并"候选并交给调用方做存在性验证。
fn decode_project_key(key: &str) -> Vec<PathBuf> {
    let inner = key.trim().trim_start_matches("--").trim_end_matches("--");
    if inner.is_empty() || inner == "root" || inner == "_no-cwd" {
        return Vec::new();
    }
    let parts: Vec<&str> = inner.split('-').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Vec::new();
    }
    // 首段为盘符（单字母）时，候选 = 盘符 + 其余段的各种合并
    let drive = if parts[0].len() == 1 && parts[0].chars().all(|c| c.is_ascii_alphabetic()) {
        format!("{}:\\", parts[0])
    } else {
        String::new()
    };
    let rest = if drive.is_empty() { &parts[..] } else { &parts[1..] };
    if rest.is_empty() {
        // 盘符根 workspace（编码形如 --C--）：直接返回根目录候选（调用方做存在性验证）
        return vec![PathBuf::from(if drive.is_empty() { "C:\\" } else { drive.as_str() })];
    }
    let mut out = Vec::new();
    // 枚举合并组合：n 段之间 n-1 个可分/合点，限制组合数避免爆炸
    let n = rest.len();
    let combos: Vec<u32> = if n <= 10 {
        (0..(1u32 << n.saturating_sub(1))).collect()
    } else {
        vec![0, (1u32 << (n - 1)) - 1] // 全分开 / 全合并
    };
    for mask in combos {
        let mut segs: Vec<String> = Vec::new();
        let mut cur = rest[0].to_string();
        for i in 1..n {
            if (mask >> (i - 1)) & 1 == 1 {
                segs.push(cur);
                cur = rest[i].to_string();
            } else {
                cur.push('-');
                cur.push_str(rest[i]);
            }
        }
        segs.push(cur);
        let mut p = PathBuf::new();
        if drive.is_empty() {
            p.push("C:\\"); // 无盘符信息时按 C 盘猜测（Windows）
        } else {
            p.push(&drive);
        }
        for s in &segs {
            p.push(s);
        }
        out.push(p);
    }
    out
}

/// 扫描 ~/.dsh/sessions 下各 workspace 目录，返回包含最新会话的 workspace 路径。
/// 仅当目录真实存在时返回（解码候选 + 存在性验证）。
fn find_last_workspace() -> Option<PathBuf> {
    let dsh_home = std::env::var("DSH_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("USERPROFILE")
                .map(|h| PathBuf::from(h).join(".dsh"))
                .ok()
        })
        .unwrap_or_else(|| PathBuf::from("C:\\.dsh"));
    let sessions_root = dsh_home.join("sessions");
    let Ok(entries) = std::fs::read_dir(&sessions_root) else {
        return None;
    };
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let key = entry.file_name().to_string_lossy().to_string();
        let Ok(ses) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        for s in ses.flatten() {
            let mt = s
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().map(|(t, _)| mt > *t).unwrap_or(true) {
                best = Some((mt, key.clone()));
            }
        }
    }
    let (_, key) = best?;
    eprintln!("[dsh-ui] last session workspace key: {key}");
    // 候选路径 + 存在性验证
    for cand in decode_project_key(&key) {
        if cand.is_dir() {
            eprintln!("[dsh-ui] resolved workspace: {}", cand.display());
            return Some(cand);
        }
    }
    None
}

// ---------- HTTP 探测 ----------

/// HTTP 层探测：返回状态码（无法连接/无响应 → None）。
fn http_status(port: u16) -> Option<u16> {
    let addr = format!("127.0.0.1:{port}");
    let mut sock = TcpStream::connect_timeout(
        &addr.parse().unwrap_or_else(|_| "127.0.0.1:1".parse().unwrap()),
        Duration::from_millis(800),
    )
    .ok()?;
    let _ = sock.set_read_timeout(Some(Duration::from_millis(800)));
    let req = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    sock.write_all(req.as_bytes()).ok()?;
    let mut buf = [0u8; 128];
    let n = sock.read(&mut buf).ok()?;
    let head = String::from_utf8_lossy(&buf[..n]);
    // 形如 "HTTP/1.1 200 OK"
    let mut parts = head.split_whitespace();
    let _proto = parts.next()?;
    parts.next()?.parse::<u16>().ok()
}

/// 服务是否已响应：dsh ≥ 0.1.2-rc.1 引入浏览器 token 认证后，无 token/cookie 的
/// `GET /` 返回 401（认证门存在 = 服务已就绪，UI 由带 token 的 URL 完成换 cookie）；
/// 旧版返回 200。因此 200 / 30x / 401 均视为"HTTP 服务活着"。
fn http_responsive(port: u16) -> bool {
    matches!(http_status(port), Some(200) | Some(401) | Some(302) | Some(303))
}

/// 从终端输出提取 dsh 打印的 Web URL（`dsh web: http://127.0.0.1:3080/?token=…`）。
/// dsh ≥ 0.1.2-rc.1 打印带 launch token 的 URL，iframe 用它首访换取认证 cookie；
/// 只认 loopback（127.0.0.1 / localhost），忽略 ` (LAN: …)` 等尾巴。
fn extract_dsh_web_url(text: &str) -> Option<String> {
    extract_url_after(text, "dsh web:")
}

/// 在 `needle` 之后提取第一个 loopback http URL（`extract_dsh_web_url` 的实现）。
fn extract_url_after(text: &str, needle: &str) -> Option<String> {
    let mut from = 0;
    while let Some(rel) = text[from..].find(needle) {
        let start = from + rel + needle.len();
        let rest = text[start..].trim_start();
        let Some(rest) = rest.strip_prefix("http://") else {
            from = start;
            continue;
        };
        let end = rest
            .find(|c: char| c.is_whitespace() || c == ')' || c == '(')
            .unwrap_or(rest.len());
        let authority = &rest[..end];
        if authority.starts_with("127.0.0.1:") || authority.starts_with("localhost:") {
            return Some(format!("http://{authority}"));
        }
        from = start;
    }
    None
}

/// 剥离 ANSI 转义序列（CSI / OSC / 两字符转义），保留普通文本。
/// ConPTY 会在输出流中插入光标控制序列（如 `\x1b[11X`），若不剥离会把
/// `dsh web: …?token=…` 的字面打断（甚至分片落在关键字中间）导致解析失败。
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI：参数字节 0x30–0x3F、中间字节 0x20–0x2F，最终字节 0x40–0x7E
            Some('[') => {
                for c2 in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c2) {
                        break;
                    }
                }
            }
            // OSC：直到 BEL 或 ST（ESC \）
            Some(']') => {
                let mut prev_esc = false;
                for c2 in chars.by_ref() {
                    if c2 == '\u{7}' || (prev_esc && c2 == '\\') {
                        break;
                    }
                    prev_esc = c2 == '\u{1b}';
                }
            }
            // 其余两字符转义（如 ESC(0）整体丢弃；结尾孤立 ESC 也丢弃
            Some(_) | None => {}
        }
    }
    out
}

/// 从累计输出窗口提取 dsh web 认证 URL（三级回退，容忍 ConPTY 分片）：
/// 1) 常规：剥离 ANSI 后按连续文本匹配（整行到达）；
/// 2) 去换行后匹配（分片之间只插入了换行）；
/// 3) 要素重组：从 `dsh web:` 之后分别取 loopback 端口与 token（分片之间被
///    其他输出行插入、URL 已不连续时——实测 ConPTY 会把一行拆成多片并在中间
///    投递子进程日志）。仅在 1)/2) 都拿不到**含 token 的** URL 时兜底。
fn extract_dsh_web_url_loose(window: &str) -> Option<String> {
    let plain = strip_ansi(window);
    let with_token = |u: Option<String>| u.filter(|u| u.contains("token="));
    if let Some(u) = with_token(extract_dsh_web_url(&plain)) {
        return Some(u);
    }
    let joined: String = plain.lines().map(str::trim).collect::<Vec<_>>().join("");
    if let Some(u) = with_token(extract_url_after(&joined, "dsh web:")) {
        return Some(u);
    }
    let after = plain.split_once("dsh web:")?.1;
    // 回退重组仅适用于 loopback 打印行，否则会把 LAN URL（`(LAN: http://192.168…）`）
    // 误重组成本机地址
    if !after.contains("127.0.0.1") && !after.contains("localhost") {
        return None;
    }
    let port = extract_loopback_port(after)?;
    let token = extract_token_value(after)?;
    Some(format!("http://127.0.0.1:{port}/?token={token}"))
}

/// 取 loopback 端口，按可靠性递减：
/// 1) `127.0.0.1:<port>` / `localhost:<port>`（连续形态）；
/// 2) `<port>/?token=`（分片后端口与被拆出的 token 相邻）；
/// 3) 首个 3–5 位数字串，**排除 IP 段**（前后不得为 `.`，避免把 `127.0.0.1` 的 `127` 当端口）。
fn extract_loopback_port(s: &str) -> Option<u16> {
    for prefix in ["127.0.0.1:", "localhost:"] {
        if let Some(i) = s.find(prefix) {
            let digits: String = s[i + prefix.len()..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(p) = digits.parse::<u16>() {
                if p > 0 {
                    return Some(p);
                }
            }
        }
    }
    if let Some(ti) = s.find("?token=") {
        let head = s[..ti].strip_suffix('/').unwrap_or(&s[..ti]);
        let digits: String = head
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if let Ok(p) = digits.parse::<u16>() {
            if p > 0 {
                return Some(p);
            }
        }
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let digits = &s[start..i]; // ASCII 数字边界，切片安全
            let prev = if start > 0 { &s[start - 1..start] } else { "" };
            let next = if i < s.len() { &s[i..i + 1] } else { "" };
            if prev != "." && next != "." && (3..=5).contains(&digits.len()) {
                if let Ok(p) = digits.parse::<u16>() {
                    if p > 0 {
                        return Some(p);
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// 取 token 值：优先 `?token=` 形式，退化为任意 `token=`。
fn extract_token_value(s: &str) -> Option<String> {
    let read_value = |at: usize| -> Option<String> {
        let v: String = s[at..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'))
            .collect();
        if v.is_empty() {
            None
        } else {
            Some(v)
        }
    };
    if let Some(i) = s.find("?token=") {
        if let Some(v) = read_value(i + "?token=".len()) {
            return Some(v);
        }
    }
    if let Some(i) = s.find("token=") {
        if let Some(v) = read_value(i + "token=".len()) {
            return Some(v);
        }
    }
    None
}

// ---------- dsh 可用性检测 ----------

/// 检测当前系统是否已安装 dsh：PATH 中存在 dsh 命令，或 npm `_npx` 缓存 /
/// pnpm dlx 缓存 / pnpm store 链接中已有 `@deepseek-ai/dsh` 包。
/// 新环境首次运行（皆无）时 pnpm dlx 需要下载，耗时可能数分钟到数十分钟。
fn dsh_available() -> bool {
    // 1. PATH 中存在 dsh 可执行文件
    if let Ok(out) = std::process::Command::new("where").arg("dsh").output() {
        if out.status.success() {
            eprintln!("[dsh-ui] dsh found in PATH");
            return true;
        }
    }
    let local = std::env::var("LOCALAPPDATA").map(PathBuf::from).ok();
    // 2. npm 缓存 _npx 目录下存在 @deepseek-ai/dsh 包（npx 已下载过即复用）
    let cache = std::env::var("npm_config_cache")
        .ok()
        .map(PathBuf::from)
        .or_else(|| local.clone().map(|d| d.join("npm-cache")));
    if let Some(cache) = cache {
        let npx_dir = cache.join("_npx");
        if let Ok(entries) = std::fs::read_dir(&npx_dir) {
            for entry in entries.flatten() {
                let pkg = entry
                    .path()
                    .join("node_modules")
                    .join("@deepseek-ai")
                    .join("dsh");
                if pkg.join("package.json").is_file() {
                    eprintln!("[dsh-ui] dsh found in npx cache: {}", pkg.display());
                    return true;
                }
            }
        }
    }
    // 3. pnpm 缓存（0.1.22 起默认启动命令为 pnpm dlx）：
    //    dlx 临时环境：%LOCALAPPDATA%\pnpm-cache\dlx\<hash>\<sub>\node_modules\@deepseek-ai\dsh
    if let Some(local) = &local {
        let dlx_root = local.join("pnpm-cache").join("dlx");
        if let Ok(entries) = std::fs::read_dir(&dlx_root) {
            for e in entries.flatten() {
                if let Ok(subs) = std::fs::read_dir(e.path()) {
                    for s in subs.flatten() {
                        let pkg = s
                            .path()
                            .join("node_modules")
                            .join("@deepseek-ai")
                            .join("dsh");
                        if pkg.join("package.json").is_file() {
                            eprintln!("[dsh-ui] dsh found in pnpm dlx cache: {}", pkg.display());
                            return true;
                        }
                    }
                }
            }
        }
        // pnpm store 链接：%LOCALAPPDATA%\pnpm\store\v11\links\@deepseek-ai\dsh
        let store_pkg = local
            .join("pnpm")
            .join("store")
            .join("v11")
            .join("links")
            .join("@deepseek-ai")
            .join("dsh")
            .join("package.json");
        if store_pkg.is_file() {
            eprintln!("[dsh-ui] dsh found in pnpm store: {}", store_pkg.display());
            return true;
        }
    }
    eprintln!("[dsh-ui] dsh not found: first-run download expected");
    false
}

// ---------- 代理 ----------

/// 从 `reg query` 输出解析系统代理：ProxyEnable=1 且 ProxyServer 非空。
fn parse_reg_proxy_output(output: &str) -> Option<String> {
    let mut enabled = false;
    let mut server = String::new();
    for line in output.lines() {
        let lower = line.to_lowercase();
        if lower.contains("proxyenable") {
            // 行尾形如 "0x1"（REG_DWORD 十六进制）
            enabled = line
                .split_whitespace()
                .last()
                .map(|v| v.trim_start_matches("0x").parse::<u32>().unwrap_or(0) != 0)
                .unwrap_or(false);
        } else if lower.contains("proxyserver") {
            // "ProxyServer  REG_SZ  127.0.0.1:10808"（值可能含空格）
            if let Some(idx) = line.find("REG_SZ") {
                server = line[idx + "REG_SZ".len()..].trim().to_string();
            }
        }
    }
    if enabled && !server.is_empty() {
        Some(server)
    } else {
        None
    }
}

/// 读取 Windows 系统代理（WinINET / IE 设置，HKCU）：
/// ProxyEnable=1 时返回 ProxyServer 原始值
/// （可能是 `host:port`，或 `http=...;https=...;...` 多协议格式）。
fn system_proxy() -> Option<String> {
    let out = std::process::Command::new("reg")
        .args([
            "query",
            "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_reg_proxy_output(&String::from_utf8_lossy(&out.stdout))
}

/// 归一化代理地址：裸 `host:port` → `http://host:port`；
/// `http=...;https=...;` 多协议串取 http/https 段。
fn normalize_proxy(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.contains('=') {
        // 多协议串：取 http=/https= 段
        for seg in raw.split(';') {
            let seg = seg.trim();
            if let Some((k, v)) = seg.split_once('=') {
                let k = k.trim();
                if (k.eq_ignore_ascii_case("http") || k.eq_ignore_ascii_case("https"))
                    && !v.trim().is_empty()
                {
                    let v = v.trim();
                    return Some(if v.contains("://") {
                        v.to_string()
                    } else {
                        format!("http://{v}")
                    });
                }
            }
        }
        return None;
    }
    Some(if raw.contains("://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    })
}

/// 按设置应用代理环境变量（HTTP_PROXY/HTTPS_PROXY 及小写变体），
/// 供 npx/npm 下载使用；无代理时清除，避免沿用陈旧值。
fn apply_proxy_env(settings: &Settings) {
    let proxy = if settings.use_system_proxy {
        system_proxy().and_then(|s| normalize_proxy(&s))
    } else if !settings.proxy_url.trim().is_empty() {
        normalize_proxy(&settings.proxy_url)
    } else {
        None
    };
    match &proxy {
        Some(p) => {
            eprintln!("[dsh-ui] proxy env -> {p}");
            std::env::set_var("HTTP_PROXY", p);
            std::env::set_var("HTTPS_PROXY", p);
            std::env::set_var("http_proxy", p);
            std::env::set_var("https_proxy", p);
        }
        None => {
            eprintln!("[dsh-ui] proxy: none");
            for k in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
                std::env::remove_var(k);
            }
        }
    }
}

// ---------- 启动流程 ----------

/// 端口是否已被监听（TCP 层）：有监听即视为占用，直接连接
fn port_listening(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}");
    TcpStream::connect_timeout(
        &addr.parse().unwrap_or_else(|_| "127.0.0.1:1".parse().unwrap()),
        Duration::from_millis(800),
    )
    .is_ok()
}

/// 从 `netstat -ano` 输出解析"监听指定端口"的 PID 列表（去重）。
/// 行格式（Windows netstat 固定英文输出）：
///   TCP    127.0.0.1:3080    0.0.0.0:0    LISTENING    12345
///   TCP    [::]:3080         [::]:0        LISTENING    12345
/// 匹配 `:{port}` 后必须是空白/行尾，避免 30800 之类误命中。
fn parse_listening_pids(output: &str, port: u16) -> Vec<u32> {
    let needle = format!(":{port}");
    let mut pids: Vec<u32> = Vec::new();
    for line in output.lines() {
        let lower = line.to_lowercase();
        if !lower.contains("listening") {
            continue;
        }
        // 逐段查找 :port，确认后一位是空白或行尾
        let mut idx = 0;
        let mut hit = false;
        while let Some(rel) = lower[idx..].find(&needle) {
            let pos = idx + rel;
            let after = &lower[pos + needle.len()..];
            if after.is_empty() || after.starts_with(|c: char| c.is_whitespace()) {
                hit = true;
                break;
            }
            idx = pos + needle.len();
        }
        if !hit {
            continue;
        }
        // 行尾 token 是 PID（ASCII 数字）
        if let Some(pid) = line
            .split_whitespace()
            .last()
            .and_then(|t| t.parse::<u32>().ok())
        {
            if !pids.contains(&pid) {
                pids.push(pid);
            }
        }
    }
    pids
}

/// 按端口结束进程：netstat 找监听 PID → taskkill /T /F 杀整树。
/// 用于结束 keep-alive 后台服务（脱离客户端的进程树无法经 JobObject 清理）。
/// 竞态处理：当前会话树刚被 JobObject 整树终止时，端口释放是异步的——
/// 若 netstat 找不到监听进程，须再确认端口是否已释放；已释放视为成功（无需按端口杀），
/// 否则短暂重试后仍找不到才报错。
fn kill_by_port(port: u16) -> Result<(), String> {
    for attempt in 0..3 {
        let out = std::process::Command::new("netstat")
            .args(["-ano"])
            .output()
            .map_err(|e| format!("无法执行 netstat：{e}"))?;
        let pids = parse_listening_pids(&String::from_utf8_lossy(&out.stdout), port);
        if !pids.is_empty() {
            for pid in &pids {
                eprintln!("[dsh-ui] killing pid {pid} (port {port})");
                match std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .status()
                {
                    Ok(st) if st.success() => {}
                    Ok(st) => eprintln!("[dsh-ui] taskkill {pid} exited {st:?}"),
                    Err(e) => eprintln!("[dsh-ui] taskkill {pid} failed: {e}"),
                }
            }
            if port_listening(port) {
                return Err(format!("端口 {port} 仍被监听（进程可能无权限结束）"));
            }
            return Ok(());
        }
        // 未找到监听进程：端口已释放 = 进程已被整树终止，无需按端口杀
        if !port_listening(port) {
            return Ok(());
        }
        eprintln!("[dsh-ui] kill_by_port: port {port} still held, retry {attempt}");
        std::thread::sleep(Duration::from_millis(300));
    }
    Err(format!("未找到监听端口 {port} 的进程"))
}

/// 终端输出中的失败特征（小写匹配）：命中即快速失败，不必干等超时
const FAIL_PATTERNS: &[&str] = &[
    "eaddrinuse",
    "is not recognized",
    "command not found",
    "npm error",
    "npm err!",
    "fatal error",
    "unhandled exception",
];

/// 终端输出中的下载/安装特征（小写匹配）：命中说明 pnpm/npm 正在大量拉取依赖，
/// 就绪超时自动放宽（首次安装可能数分钟到数十分钟）
const DOWNLOAD_PATTERNS: &[&str] = &[
    "progress: resolved", // pnpm 依赖解析进度（"Progress: resolved X, reused Y, downloaded Z"）
    "packages: +",        // pnpm 安装进度（"Packages: +123"）
    "added ",             // npm/pnpm 安装完成（"added N packages"）
];

pub fn boot(app: &AppHandle, inner: &Arc<Mutex<AppInner>>) {
    let settings = { inner.lock().unwrap().settings.clone() };
    let port = settings.port;
    // 端口已被占用 → 直接连接（不区分占用者是否为 dsh 服务，
    // 也无需 HTTP 200：TCP 有监听即直连，避免误启动撞 EADDRINUSE）
    if port_listening(port) {
        eprintln!("[dsh-ui] port {port} occupied, connect directly");
        // 直连模式没有终端输出可捕获 token；若服务要求浏览器认证（401），
        // 壳内 cookie 缺失/失效时 iframe 会停在认证提示页 → 引导用户重启服务
        // （重启由壳启动服务，即可捕获 token 并注入 cookie）
        let auth_required = http_status(port) == Some(401);
        if auth_required {
            eprintln!("[dsh-ui] direct connect: service requires browser auth (401)");
            if let Some(t) = inner.lock().unwrap().term.as_mut() {
                let _ = t.write(
                    format!(
                        "echo [dsh-ui] 端口 {port} 的服务要求浏览器认证；请点工具栏「重新启动服务」以自动完成登录\r"
                    )
                    .as_bytes(),
                );
            }
        }
        // 仍提供终端会话（不喂启动命令），供查看/排查
        start_session(app, inner, false);
        if let Some(t) = inner.lock().unwrap().term.as_mut() {
            if let Err(e) = t.write(
                format!(
                    "echo [dsh-ui] 端口 {port} 已有服务监听，已直接连接；若页面显示异常，可能是非 DSH 服务占用该端口\r"
                )
                .as_bytes(),
            ) {
                eprintln!("[dsh-ui] write to session failed: {e}");
            }
        }
        // 401 提示须在就绪后发出（前端在 ready 态展示提示条）
        set_phase(app, inner, Phase::Ready, None);
        if auth_required {
            let _ = app.emit("auth:required", serde_json::json!({}));
        }
        start_keepalive(app, inner.clone());
        return;
    }
    // 首次运行检测（仅启动路径需要）：系统无 dsh → npx/pnpm 需下载
    // （耗时可能数分钟到数十分钟）。放在端口空闲分支内，直连时不白跑子进程与目录扫描
    let needs_download = !dsh_available();
    eprintln!("[dsh-ui] port {port} free, starting session");
    if needs_download {
        // 放宽就绪超时：首次下载可能数分钟到数十分钟
        {
            let mut g = inner.lock().unwrap();
            g.needs_download = true;
            g.ready_timeout = 1800.max(g.settings.ready_timeout_sec);
        }
    }
    start_session(app, inner, true);
    if needs_download {
        // start_session 内部会重置 message，此处补回下载提示
        set_phase(
            app,
            inner,
            Phase::Boot,
            Some("正在下载 DeepSeek Harness（首次运行，可能需要数分钟到数十分钟）…".into()),
        );
    }
    start_probe(app.clone(), inner.clone());
}

fn start_session(app: &AppHandle, inner: &Arc<Mutex<AppInner>>, feed: bool) {
    // 代理环境先于任何子进程应用（npx/npm 下载走代理）
    apply_proxy_env(&inner.lock().unwrap().settings);
    let (settings, workdir) = {
        let g = inner.lock().unwrap();
        let workdir = if g.settings.working_dir.trim().is_empty() {
            // 未手动设置工作目录：自动跟随最后会话所在的 workspace
            find_last_workspace().unwrap_or_else(|| resolve_workdir(&g.settings))
        } else {
            resolve_workdir(&g.settings)
        };
        (g.settings.clone(), workdir)
    };

    // 代次 +1，令旧探针/保活线程失效
    {
        let mut g = inner.lock().unwrap();
        g.probe_stop.store(true, Ordering::Relaxed);
        g.gen += 1;
        g.term = None;
    }

    set_phase(app, inner, Phase::Boot, None);

    let (session, reader) = match terminal::spawn(&workdir, 24, 100) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[dsh-ui] cmd spawn failed: {e}");
            set_phase(
                app,
                inner,
                Phase::Failed,
                Some(format!("无法启动 cmd 会话：{e}")),
            );
            return;
        }
    };
    eprintln!(
        "[dsh-ui] cmd session spawned, workdir={}",
        workdir.display()
    );

    // 喂入：切 UTF-8 → 进入工作目录 →（可选）执行启动命令
    let mut feed_str = String::from("chcp 65001 >nul\r");
    let wd = workdir.to_string_lossy().to_string();
    if !wd.is_empty() {
        feed_str.push_str(&format!("cd /d \"{}\"\r", wd.replace('"', "\"\"")));
    }
    if feed {
        feed_str.push_str(&settings.startup_command);
        feed_str.push('\r');
    }

    {
        let mut g = inner.lock().unwrap();
        g.probe_stop.store(false, Ordering::Relaxed);
        g.term_buffer.clear();
        g.auth_pending = false;
        g.auth_done = false;
        g.term = Some(session);
    }
    if let Some(t) = inner.lock().unwrap().term.as_mut() {
        if let Err(e) = t.write(feed_str.as_bytes()) {
            eprintln!("[dsh-ui] feed to session failed: {e}");
        }
    }

    // 读取线程：ConPTY 输出 → 前端；同时扫描失败特征
    let app2 = app.clone();
    let inner2 = inner.clone();
    let gen = { inner.lock().unwrap().gen };
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 8192];
        let mut recent = String::new();
        let mut failed_once = false;
        let mut download_seen = false;
        let mut auth_exchange_started = false;
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buf[..n]).to_string();
                    // 先累积滑窗：token 行可能被 ConPTY 分片投递（跨读块），
                    // 单块匹配会漏捕获 → 无 cookie → iframe 401
                    if !failed_once {
                        recent.push_str(&text);
                        if recent.len() > 8192 {
                            // 字节安全截断（多字节 UTF-8 不会 panic）
                            recent = trim_window(&recent, 4096);
                        }
                    }
                    // 代次校验：换代（重启/停止）后旧 reader 的残余输出不再污染新会话
                    let same_gen = { inner2.lock().unwrap().gen == gen };
                    if same_gen {
                        let _ = app2.emit("terminal:data", serde_json::json!({ "data": text }));
                        let mut g = inner2.lock().unwrap();
                        g.term_buffer.push_str(&text);
                        truncate_term_buffer(&mut g.term_buffer);
                    }
                    // dsh web 打印行：新版（≥0.1.2-rc.1）带 launch token，
                    // Rust 侧换取签名 cookie 后注入 WebView2（iframe 跨站无法自持 Strict cookie）；
                    // probe 会等待 auth_done 之后才广播 ready，确保 iframe 首载即带 cookie
                    if !failed_once && !auth_exchange_started {
                        // 用累计窗口 + 归一化匹配：token 行可能被分片投递，
                        // 且 ConPTY 会在片间插入 ANSI 控制序列/换行
                        if let Some(web_url) = extract_dsh_web_url_loose(&recent) {
                            if web_url.contains("token=") {
                                auth_exchange_started = true;
                                // 单锁内校验代次后置位（与 auth_done 对称，勿污染新代）
                                {
                                    let mut g = inner2.lock().unwrap();
                                    if g.gen == gen {
                                        g.auth_pending = true;
                                    }
                                }
                                let app3 = app2.clone();
                                let gen_auth = gen;
                                std::thread::spawn(move || {
                                    let Some((cname, cvalue, domain)) =
                                        crate::auth::exchange_cookie(&web_url)
                                    else {
                                        // 不打印完整 URL：query 中的 token 是凭据
                                        let host = web_url
                                            .strip_prefix("http://")
                                            .and_then(|r| r.split_once('/').map(|(a, _)| a))
                                            .unwrap_or("?");
                                        eprintln!(
                                            "[dsh-ui] cookie exchange failed for http://{host}"
                                        );
                                        return;
                                    };
                                    eprintln!(
                                        "[dsh-ui] cookie exchanged, injecting (domain={domain})"
                                    );
                                    if let Some(win) = app3.get_webview_window("main") {
                                        let app4 = app3.clone();
                                        let done = std::sync::Arc::new(move |ok: bool| {
                                            if ok {
                                                {
                                                    let state =
                                                        app4.state::<Arc<Mutex<AppInner>>>();
                                                    let mut g = state.lock().unwrap();
                                                    // 代次校验：仅当本代会话仍有效时置位
                                                    if g.gen == gen_auth {
                                                        g.auth_done = true;
                                                    }
                                                }
                                                eprintln!("[dsh-ui] auth cookie injected");
                                            } else {
                                                eprintln!("[dsh-ui] auth cookie injection failed");
                                            }
                                        });
                                        crate::auth::inject(&win, cname, cvalue, domain, done);
                                    }
                                });
                            }
                        }
                    }
                    if failed_once {
                        continue;
                    }
                    let lower = recent.to_lowercase();
                    // 下载/安装特征：pnpm/npm 大量拉取依赖 → 放宽就绪超时（首次安装慢）。
                    // 单锁内完成 gen 校验与三字段更新（避免两段锁之间的 TOCTOU）
                    if !download_seen && DOWNLOAD_PATTERNS.iter().any(|p| lower.contains(*p)) {
                        let mut g = inner2.lock().unwrap();
                        if g.gen == gen {
                            download_seen = true;
                            g.ready_timeout = 1800.max(g.ready_timeout);
                            g.needs_download = true;
                            eprintln!(
                                "[dsh-ui] download in progress: ready timeout extended to {}s",
                                g.ready_timeout
                            );
                        }
                    }
                    if let Some(p) = FAIL_PATTERNS.iter().find(|p| lower.contains(**p)) {
                        failed_once = true;
                        if inner2.lock().unwrap().gen == gen {
                            eprintln!("[dsh-ui] failure pattern in output: {p}");
                            inner2.lock().unwrap().probe_stop.store(true, Ordering::Relaxed);
                            set_phase(
                                &app2,
                                &inner2,
                                Phase::Failed,
                                Some(format!("启动失败（终端输出含 \"{p}\"），详见终端")),
                            );
                        }
                        // 继续读取，终端保持可交互查看
                    }
                }
                Err(_) => break,
            }
        }
        let _ = app2.emit("terminal:exit", serde_json::json!({ "code": null }));
    });
}

fn start_probe(app: AppHandle, inner: Arc<Mutex<AppInner>>) {
    std::thread::spawn(move || {
        let (port, gen) = {
            let g = inner.lock().unwrap();
            (g.settings.port, g.gen)
        };
        let started = Instant::now();
        loop {
            // 一次锁内读取 stop / gen / 生效超时（下载特征命中时超时会动态放宽）
            let (stop, stale, timeout) = {
                let g = inner.lock().unwrap();
                (g.probe_stop.clone(), g.gen != gen, g.ready_timeout)
            };
            if stop.load(Ordering::Relaxed) || stale {
                return;
            }
            if http_responsive(port) {
                if inner.lock().unwrap().gen != gen {
                    return;
                }
                // 认证处理（dsh ≥ 0.1.2-rc.1）：
                // HTTP 首见就绪可能早于 reader 解析到 token 打印行——此时 auth_pending
                // 尚未置位，与"旧版无需认证"不可区分。首次 HTTP 就绪后进入短暂 grace，
                // 期间若 reader 置位 auth_pending 则转入 cookie 等待；grace 过后仍未置位
                // 视为旧版（无认证），直接就绪。
                let (auth_pending, auth_done) = {
                    let g = inner.lock().unwrap();
                    (g.auth_pending, g.auth_done)
                };
                if !auth_pending && !auth_done {
                    if started.elapsed() < Duration::from_millis(2500) {
                        // grace：给 reader 时间解析 dsh web 打印行
                        std::thread::sleep(Duration::from_millis(300));
                        continue;
                    }
                } else if auth_pending && !auth_done {
                    // 已捕获 token：等待注入完成（上限 8s，避免注入失败拖死启动）
                    if started.elapsed() < Duration::from_secs(timeout.min(8)) {
                        std::thread::sleep(Duration::from_millis(300));
                        continue;
                    }
                    eprintln!("[dsh-ui] auth cookie wait timed out, proceeding");
                }
                // 单锁复查代次与停止标志后提交 Ready（避免过期探针污染新代）
                {
                    let mut g = inner.lock().unwrap();
                    if g.gen != gen || g.probe_stop.load(Ordering::Relaxed) {
                        return;
                    }
                    g.restart_count = 0;
                }
                eprintln!("[dsh-ui] ready (probe ok)");
                set_phase(&app, &inner, Phase::Ready, None);
                start_keepalive(&app, inner);
                return;
            }
            // 进程提前退出 → 快速失败（如 dsh 启动即报错）
            if !child_alive(&inner) {
                eprintln!("[dsh-ui] child exited during boot");
                set_phase(
                    &app,
                    &inner,
                    Phase::Failed,
                    Some("启动失败：服务进程已退出".into()),
                );
                return;
            }
            if Instant::now() >= started + Duration::from_secs(timeout) {
                let alive = child_alive(&inner);
                let downloading = { inner.lock().unwrap().needs_download };
                set_phase(
                    &app,
                    &inner,
                    Phase::Failed,
                    Some(if !alive {
                        "启动失败：服务进程已退出".into()
                    } else if downloading {
                        format!(
                            "启动超时（{timeout}s）：端口 {port} 未就绪。首次运行需下载 DeepSeek Harness，若长时间停留在下载阶段，请检查网络与代理设置"
                        )
                    } else {
                        format!("启动超时（{timeout}s）：端口 {port} 未就绪",)
                    }),
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

fn start_keepalive(app: &AppHandle, inner: Arc<Mutex<AppInner>>) {
    let app = app.clone();
    std::thread::spawn(move || {
        let port = { inner.lock().unwrap().settings.port };
        let gen = { inner.lock().unwrap().gen };
        let mut fails = 0u32;
        loop {
            std::thread::sleep(Duration::from_secs(2));
            // 退出语义：restart() 会置 probe_stop=true 并 gen+=1，
            // 这里一次锁内读两者；任一命中即退，双保险覆盖重启竞态窗口
            let (stop, stale) = {
                let g = inner.lock().unwrap();
                (g.probe_stop.clone(), g.gen != gen)
            };
            if stop.load(Ordering::Relaxed) || stale {
                return;
            }
            if port_listening(port) {
                fails = 0;
                continue;
            }
            fails += 1;
            if !child_alive(&inner) {
                eprintln!("[dsh-ui] keepalive: child exited");
                fail_or_restart(&app, &inner, "服务进程已退出");
                return;
            }
            if fails >= 3 {
                eprintln!("[dsh-ui] keepalive: no response");
                fail_or_restart(&app, &inner, "服务无响应");
                return;
            }
        }
    });
}

fn child_alive(inner: &Arc<Mutex<AppInner>>) -> bool {
    let mut g = inner.lock().unwrap();
    match g.term.as_mut() {
        Some(t) => t.alive(),
        None => false,
    }
}

// ---------- 重启 ----------

/// 失败处理：auto_restart 开启且未超上限 → 自动重启；否则进入 Failed
fn fail_or_restart(app: &AppHandle, inner: &Arc<Mutex<AppInner>>, msg: &str) {
    let (auto, count) = {
        let g = inner.lock().unwrap();
        (g.settings.auto_restart, g.restart_count)
    };
    if auto && count < 3 {
        eprintln!("[dsh-ui] auto restart ({count}/3): {msg}");
        restart(app, inner);
    } else {
        set_phase(app, inner, Phase::Failed, Some(msg.into()));
    }
}

pub fn restart(app: &AppHandle, inner: &Arc<Mutex<AppInner>>) {
    {
        let mut g = inner.lock().unwrap();
        g.probe_stop.store(true, Ordering::Relaxed);
        g.gen += 1;
        g.restart_count += 1;
        if let Some(mut term) = g.term.take() {
            term.kill();
            // JobObject 句柄随 term drop 关闭 → 整树终止
        }
    }
    set_phase(app, inner, Phase::Boot, Some("正在重新启动…".into()));
    start_session(app, inner, true);
    start_probe(app.clone(), inner.clone());
}

// ---------- 手动停止 ----------

/// 停止服务（终端工具栏"停止"按钮）：
/// 1. 杀当前 cmd 会话进程树（JobObject 整树终止）
/// 2. 兜底：端口仍被监听（keep-alive 后台服务 / 外部进程）→ 按端口杀
/// 进入 Stopped 状态，可再点"重新启动"恢复。
pub fn stop(app: &AppHandle, inner: &Arc<Mutex<AppInner>>) -> Result<(), String> {
    let port = {
        let mut g = inner.lock().unwrap();
        g.probe_stop.store(true, Ordering::Relaxed);
        g.gen += 1;
        if let Some(mut term) = g.term.take() {
            term.kill();
            // JobObject 句柄随 term drop 关闭 → 整树终止
        }
        g.settings.port
    };
    // 当前会话树已杀；若端口仍被监听（detached keep-alive 服务），按端口杀
    let mut port_err: Option<String> = None;
    if port_listening(port) {
        if let Err(e) = kill_by_port(port) {
            port_err = Some(e);
        }
    }
    let msg = match &port_err {
        Some(e) => format!("服务已停止，但端口 {port} 仍被监听：{e}"),
        None => "服务已手动停止".into(),
    };
    set_phase(app, inner, Phase::Stopped, Some(msg.clone()));
    match port_err {
        Some(_) => Err(msg),
        None => Ok(()),
    }
}

// ---------- 退出 ----------

/// 主窗口关闭：勾选"退出后保持服务运行"时询问用户是否同时结束后台服务。
pub fn handle_close(app: &AppHandle) {
    eprintln!("[dsh-ui] main window close requested");
    let keep_alive = app
        .state::<Arc<Mutex<AppInner>>>()
        .lock()
        .unwrap()
        .settings
        .keep_alive_on_exit;
    if !keep_alive {
        // 进程退出时 JobObject 自动清理 cmd 进程树
        return;
    }
    // 询问：保持运行（默认）还是同时结束后台服务
    if ask_keep_alive(app) {
        // 用户选择「保持运行」
        let state = app.state::<Arc<Mutex<AppInner>>>();
        let port = { state.lock().unwrap().settings.port };
        // 1) 先停当前会话树（服务若在客户端树内 → 释放端口，避免 detached 撞 EADDRINUSE）
        {
            let mut g = state.lock().unwrap();
            g.probe_stop.store(true, Ordering::Relaxed);
            g.gen += 1;
            if let Some(mut term) = g.term.take() {
                term.kill();
                // JobObject 句柄随 term drop 关闭 → 整树终止
            }
        }
        // 2) 等待端口释放（最多 ~5s）
        let release_deadline = Instant::now() + Duration::from_secs(5);
        while port_listening(port) && Instant::now() < release_deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        // 3) 端口仍被监听（既有 detached/外部服务存活）→ 直接保持，不再重复启动
        if port_listening(port) {
            eprintln!(
                "[dsh-ui] keep-alive: port {port} already served by a surviving process; skip relaunch"
            );
            return;
        }
        // 4) 端口已释放：脱离作业，另起一个独立 cmd 会话
        let (cmdline, log_path) = {
            let state = app.state::<Arc<Mutex<AppInner>>>();
            let g = state.lock().unwrap();
            let settings = g.settings.clone();
            // 后台服务同样需要代理环境（下载/更新场景）
            apply_proxy_env(&settings);
            let workdir = resolve_workdir(&settings);
            // 便携模式日志放程序目录，否则 %APPDATA% 下
            let log = if g.config_dir.join("portable.marker").exists() {
                g.config_dir.join("dsh-service.log")
            } else {
                app.path()
                    .app_log_dir()
                    .map(|d| d.join("dsh-service.log"))
                    .unwrap_or_else(|_| PathBuf::from("dsh-service.log"))
            };
            let cmd = format!(
                "cd /d \"{}\" && {} > \"{}\" 2>&1",
                workdir.to_string_lossy().replace('"', "\"\""),
                settings.startup_command,
                log.to_string_lossy().replace('"', "\"\"")
            );
            (cmd, log)
        };
        if let Some(dir) = log_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // 脱离作业的后台进程：cmd /c start "" /b cmd /c "<命令>"（不占用当前进程树）
        match std::process::Command::new("cmd")
            .args(["/c", "start", "\"\"", "/b", "cmd", "/c", &cmdline])
            .spawn()
        {
            Ok(_) => eprintln!("[dsh-ui] keep-alive service launched, log: {}", log_path.display()),
            Err(e) => {
                eprintln!("[dsh-ui] keep-alive launch failed: {e}");
                let _ = app.emit("keepalive:failed", serde_json::json!({ "error": e.to_string() }));
            }
        }
    } else {
        // 用户选择「结束服务」：detached 进程不在本客户端进程树内，按端口杀
        let port = { app.state::<Arc<Mutex<AppInner>>>().lock().unwrap().settings.port };
        if port_listening(port) {
            match kill_by_port(port) {
                Ok(()) => eprintln!("[dsh-ui] background service ended on close"),
                Err(e) => eprintln!("[dsh-ui] failed to end background service: {e}"),
            }
        }
    }
}

/// 关闭确认对话框：返回 true = 用户选择「保持运行」（默认按钮），false = 同时结束后台服务。
///
/// 关于 `blocking_show` 不会死锁的依据（属依赖实现细节，升级 tauri-plugin-dialog 后需复核）：
/// 插件内部把对话框派发到独立线程执行 rfd 的 `AsyncMessageDialog`，并通过 `sync_channel`
/// 把结果回传；Windows 上原生模态对话框自带消息泵。主线程在等待期间仍可处理窗口消息，
/// 因此阻塞等待不会与 WebView/事件循环互相等待。本机退出流程实测可用（两按钮语义正确）。
fn ask_keep_alive(app: &AppHandle) -> bool {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    app.dialog()
        .message(
            "已勾选“退出后保持服务运行”。\n\n是否在退出时同时结束后台服务进程？\n· 保持运行：下次启动直接连接现有服务\n· 结束服务：下次启动将重新启动服务",
        )
        .title("退出 DeepSeek Harness")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "保持运行".into(),
            "结束服务".into(),
        ))
        .blocking_show()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_project_key_generates_drive_candidates() {
        // --C-code-dsh-ui-- 的分段合并候选应包含 C:\code\dsh-ui（存在性验证在调用方做）
        let candidates = decode_project_key("--C-code-dsh-ui--");
        let expected = PathBuf::from("C:\\").join("code").join("dsh-ui");
        assert!(candidates.contains(&expected), "candidates: {candidates:?}");
        // 全分开的候选也应存在（C:\code\dsh\ui）
        let split = PathBuf::from("C:\\").join("code").join("dsh").join("ui");
        assert!(candidates.contains(&split), "candidates: {candidates:?}");
    }

    #[test]
    fn decode_project_key_handles_empty_and_special() {
        assert!(decode_project_key("").is_empty());
        assert!(decode_project_key("--root--").is_empty());
        assert!(decode_project_key("--_no-cwd--").is_empty());
        assert!(decode_project_key("--a-b-c--").len() >= 2);
    }

    #[test]
    fn decode_project_key_drive_root_does_not_panic() {
        // 盘符根 workspace（--C--）：返回根候选，不得越界 panic
        let candidates = decode_project_key("--C--");
        assert_eq!(candidates, vec![PathBuf::from("C:\\")]);
        // 单段非盘符（如 --tmp--）：不 panic 且给出候选
        assert!(decode_project_key("--code--").len() >= 1);
    }

    #[test]
    fn truncate_buffer_respects_utf8_boundaries() {
        // 多字节字符（"测" 3 字节）反复填充后截断，不得 panic 且输出为合法 UTF-8
        let mut buf = String::new();
        for _ in 0..40000 {
            buf.push_str("测试中文输出📦 ");
        }
        truncate_term_buffer(&mut buf);
        assert!(buf.len() <= TERM_BUFFER_MAX);
        assert!(std::str::from_utf8(buf.as_bytes()).is_ok());
    }

    #[test]
    fn truncate_buffer_keeps_small_buffers_untouched() {
        let mut buf = String::from("short");
        truncate_term_buffer(&mut buf);
        assert_eq!(buf, "short");
    }

    #[test]
    fn trim_window_respects_utf8_boundaries_and_length() {
        // 多字节字符（"测" 3 字节）填充，截断后不 panic 且为合法 UTF-8
        let mut s = String::new();
        for _ in 0..5000 {
            s.push_str("测试中文输出📦 ");
        }
        let out = trim_window(&s, 4096);
        assert!(out.len() <= 4096 + 8, "len {}", out.len());
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        // 短字符串原样返回
        assert_eq!(trim_window("short", 4096), "short");
        // 保留的是后半段
        let t = trim_window("abcdefgh", 4);
        assert_eq!(t, "efgh");
    }

    #[test]
    fn extracts_dsh_web_url_with_token() {
        // 0.1.2-rc.1 起的带 token 打印行（含 LAN 尾巴）→ 取 loopback token URL
        let line = "dsh web: http://127.0.0.1:3080/?token=KDEY0TIaKfL0nJ88 (LAN: http://192.168.1.5:3080/?token=XYZ)";
        assert_eq!(
            extract_dsh_web_url(line).as_deref(),
            Some("http://127.0.0.1:3080/?token=KDEY0TIaKfL0nJ88")
        );
    }

    #[test]
    fn strip_ansi_removes_control_sequences() {
        assert_eq!(strip_ansi("a\u{1b}[11Xb"), "ab");
        assert_eq!(strip_ansi("a\u{1b}[K b"), "a b");
        assert_eq!(strip_ansi("a\u{1b}]0;title\u{7}b"), "ab");
        assert_eq!(strip_ansi("a\u{1b}]0;title\u{1b}\\b"), "ab");
        assert_eq!(strip_ansi("plain"), "plain");
        // 多字节内容不受影响
        assert_eq!(strip_ansi("中文\u{1b}[2J输出"), "中文输出");
    }

    #[test]
    fn extracts_dsh_web_url_across_chunks_and_ansi() {
        // 模拟真实故障（ConPTY 取证形态）：token 行被拆成三片，中间插入子进程日志行
        let window = "C:\\>node app.js\r\ndsh web: http://127.0.0.1:\u{1b}[15X\
svc listening on 3999 (token=SPLIT123)\r\n3999/?token=SPLIT123\r\n";
        assert_eq!(
            extract_dsh_web_url_loose(window).as_deref(),
            Some("http://127.0.0.1:3999/?token=SPLIT123")
        );
        // 中间插入控制序列、只有换行分隔
        let window2 = "dsh web: http://127.0.0.1:3999/?to\u{1b}[11Xken=ABC\u{1b}[K\r\n";
        assert_eq!(
            extract_dsh_web_url_loose(window2).as_deref(),
            Some("http://127.0.0.1:3999/?token=ABC")
        );
        // 片间插入了换行（URL 本体仍连续）
        let window3 = "dsh web: http://127.0.0.1:3999/?tok\r\nen=XYZ\r\n";
        assert_eq!(
            extract_dsh_web_url_loose(window3).as_deref(),
            Some("http://127.0.0.1:3999/?token=XYZ")
        );
        // 常规整行（含 LAN 尾巴）
        let window4 =
            "dsh web: http://127.0.0.1:3080/?token=KDEY (LAN: http://192.168.1.5:3080/?token=Q)";
        assert_eq!(
            extract_dsh_web_url_loose(window4).as_deref(),
            Some("http://127.0.0.1:3080/?token=KDEY")
        );
        // 非 loopback / 无 token / 无该行 → None
        assert_eq!(extract_dsh_web_url_loose("dsh web: http://192.168.1.5:3080/"), None);
        assert_eq!(
            extract_dsh_web_url_loose("dsh web: http://192.168.1.5:3080/?token=LANONLY"),
            None,
            "LAN-only 行不得被重组成本机地址"
        );
        assert_eq!(extract_dsh_web_url_loose("dsh web: http://127.0.0.1:3080/"), None);
        assert_eq!(extract_dsh_web_url_loose("普通日志输出"), None);
    }

    #[test]
    fn extracts_dsh_web_url_legacy_clean() {
        // 旧版（rc.8 及更早）干净 URL 行
        assert_eq!(
            extract_dsh_web_url("dsh web: http://127.0.0.1:3080/").as_deref(),
            Some("http://127.0.0.1:3080/")
        );
    }

    #[test]
    fn ignores_non_loopback_and_noise() {
        // 纯 LAN 行不命中（无 loopback URL 前缀文本）
        assert_eq!(extract_dsh_web_url("dsh web: http://192.168.1.5:3080/"), None);
        // 无关输出
        assert_eq!(extract_dsh_web_url("C:\\Users\\x>npx --yes dsh web"), None);
        assert_eq!(extract_dsh_web_url(""), None);
        // 多行输出中命中 loopback
        let multi = "some output\r\ndsh web: http://127.0.0.1:3999/?token=T1\r\nnext line";
        assert_eq!(
            extract_dsh_web_url(multi).as_deref(),
            Some("http://127.0.0.1:3999/?token=T1")
        );
    }

    #[test]
    fn netstat_parse_finds_ipv4_and_ipv6_pids() {
        let out = "  TCP    127.0.0.1:3080    0.0.0.0:0    LISTENING    12345\r\n\
                    TCP    [::]:3080         [::]:0        LISTENING    12345\r\n\
                    TCP    127.0.0.1:30800   0.0.0.0:0     LISTENING    9999\r\n";
        let pids = parse_listening_pids(out, 3080);
        assert_eq!(pids, vec![12345], "IPv4/IPv6 同 PID 应去重，30800 不应误命中");
    }

    #[test]
    fn netstat_parse_no_match_or_garbage() {
        assert!(parse_listening_pids("", 3080).is_empty());
        assert!(parse_listening_pids("  TCP  ... no listening lines here", 3080).is_empty());
        // 乱码行（非 UTF-8 损失）不 panic、不产生 PID
        let garbage = "TCP\u{0}\u{FFFD}\u{FFFD} 127.0.0.1:3080 LISTENING \u{FFFD}";
        assert!(parse_listening_pids(garbage, 3080).is_empty());
    }

    #[test]
    fn netstat_parse_multi_pid_deduplicated() {
        let out = "  TCP    0.0.0.0:3080    0.0.0.0:0    LISTENING    111\r\n\
                    TCP    [::]:3080        [::]:0        LISTENING    222\r\n";
        let pids = parse_listening_pids(out, 3080);
        assert_eq!(pids, vec![111, 222]);
    }

    #[test]
    fn reg_proxy_enabled_parses_server() {
        let out = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings\r\n    ProxyEnable    REG_DWORD    0x1\r\n    ProxyServer    REG_SZ    127.0.0.1:10808\r\n    ProxyOverride    REG_SZ    <local>\r\n";
        assert_eq!(
            parse_reg_proxy_output(out),
            Some("127.0.0.1:10808".into())
        );
    }

    #[test]
    fn reg_proxy_disabled_or_absent_returns_none() {
        // ProxyEnable=0 → 无代理
        let disabled = "    ProxyEnable    REG_DWORD    0x0\r\n    ProxyServer    REG_SZ    127.0.0.1:10808\r\n";
        assert_eq!(parse_reg_proxy_output(disabled), None);
        // 无 ProxyServer 行
        let no_server = "    ProxyEnable    REG_DWORD    0x1\r\n";
        assert_eq!(parse_reg_proxy_output(no_server), None);
        // 乱码/空输出
        assert_eq!(parse_reg_proxy_output(""), None);
        assert_eq!(parse_reg_proxy_output("\u{FFFD}\u{FFFD}"), None);
    }

    #[test]
    fn normalize_proxy_handles_bare_url_and_multi() {
        assert_eq!(normalize_proxy("127.0.0.1:10808"), Some("http://127.0.0.1:10808".into()));
        assert_eq!(normalize_proxy("http://127.0.0.1:10808"), Some("http://127.0.0.1:10808".into()));
        assert_eq!(normalize_proxy("socks5://127.0.0.1:1080"), Some("socks5://127.0.0.1:1080".into()));
        // 多协议串取 https/http 段
        assert_eq!(
            normalize_proxy("ftp=127.0.0.1:21;https=proxy.example:8443;http=proxy.example:8080"),
            Some("http://proxy.example:8443".into())
        );
        assert_eq!(normalize_proxy(""), None);
        assert_eq!(normalize_proxy("   "), None);
        assert_eq!(normalize_proxy("ftp=127.0.0.1:21"), None);
    }
}
