use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// 串行化设置写盘：唯一临时文件 + 互斥，避免并发保存撕裂配置
static SAVE_LOCK: Mutex<()> = Mutex::new(());
static SAVE_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub startup_command: String,
    pub working_dir: String,
    pub port: u16,
    pub ready_timeout_sec: u64,
    pub zoom: f64,
    pub auto_start: bool,
    pub keep_alive_on_exit: bool,
    pub auto_restart: bool,
    pub terminal_height_ratio: f64,
    /// 下载 dsh 时使用系统代理（WinINET 设置）；关闭后使用 proxy_url
    pub use_system_proxy: bool,
    /// 自定义代理地址（如 http://127.0.0.1:10808），仅 use_system_proxy=false 时生效
    pub proxy_url: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            startup_command: "pnpm dlx @deepseek-ai/dsh@next web --no-open".into(),
            working_dir: String::new(),
            port: 3080,
            ready_timeout_sec: 120,
            zoom: 1.0,
            auto_start: true,
            keep_alive_on_exit: false,
            auto_restart: false,
            terminal_height_ratio: 0.55,
            use_system_proxy: true,
            proxy_url: String::new(),
        }
    }
}

impl Settings {
    /// 语义归一化：把非法但可表示的值收敛到合理默认（磁盘/IPC 边界兜底）。
    pub fn normalize(mut self) -> Self {
        if self.port == 0 {
            eprintln!("[dsh-ui] settings: invalid port 0, reset to 3080");
            self.port = 3080;
        }
        if !self.zoom.is_finite() {
            eprintln!("[dsh-ui] settings: invalid zoom, reset to 1.0");
            self.zoom = 1.0;
        } else {
            self.zoom = self.zoom.clamp(0.5, 3.0);
        }
        if !(self.terminal_height_ratio > 0.0) || self.terminal_height_ratio > 1.0 {
            eprintln!("[dsh-ui] settings: invalid terminal_height_ratio, reset to 0.55");
            self.terminal_height_ratio = 0.55;
        }
        if self.ready_timeout_sec == 0 {
            eprintln!("[dsh-ui] settings: invalid ready_timeout_sec 0, reset to 120");
            self.ready_timeout_sec = 120;
        }
        self
    }

    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("settings.json");
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return Settings::default(), // 文件不存在 → 默认
        };
        match serde_json::from_str::<Settings>(&text) {
            Ok(s) => s.normalize(),
            Err(e) => {
                // 文件存在但损坏：备份原文件后回退默认，避免后续 save 覆盖丢失原配置
                eprintln!("[dsh-ui] settings.json parse failed: {e}; backing up original");
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let bak = dir.join(format!("settings.json.bak-{ts}"));
                if fs::copy(&path, &bak).is_ok() {
                    eprintln!("[dsh-ui] original preserved at {}", bak.display());
                }
                Settings::default()
            }
        }
    }
}

/// 原子写盘（唯一临时文件 + 串行化；锁外调用：不持有 AppInner 状态锁）
pub fn write_settings(dir: &PathBuf, text: &str) -> Result<(), String> {
    let _guard = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = dir.join("settings.json");
    let seq = SAVE_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let tmp = dir.join(format!("settings.json.tmp-{pid}-{seq}"));
    fs::write(&tmp, text).map_err(|e| format!("写入设置失败：{e}"))?;
    if let Err(e) = fs::rename(&tmp, &path) {
        // 替换失败：清理临时文件并报告，不静默
        let _ = fs::remove_file(&tmp);
        return Err(format!("保存设置失败：{e}"));
    }
    Ok(())
}

/// IPC 层使用的设置结构：字段名 camelCase，与前端 TS 接口一致。
/// 磁盘 settings.json 保持 snake_case 不变（见 Settings）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsIpc {
    pub startup_command: String,
    pub working_dir: String,
    pub port: u16,
    pub ready_timeout_sec: u64,
    pub zoom: f64,
    pub auto_start: bool,
    pub keep_alive_on_exit: bool,
    pub auto_restart: bool,
    pub terminal_height_ratio: f64,
    pub use_system_proxy: bool,
    pub proxy_url: String,
}

impl From<SettingsIpc> for Settings {
    fn from(ipc: SettingsIpc) -> Self {
        Settings {
            startup_command: ipc.startup_command,
            working_dir: ipc.working_dir,
            port: ipc.port,
            ready_timeout_sec: ipc.ready_timeout_sec,
            zoom: ipc.zoom,
            auto_start: ipc.auto_start,
            keep_alive_on_exit: ipc.keep_alive_on_exit,
            auto_restart: ipc.auto_restart,
            terminal_height_ratio: ipc.terminal_height_ratio,
            use_system_proxy: ipc.use_system_proxy,
            proxy_url: ipc.proxy_url,
        }
    }
}

impl From<Settings> for SettingsIpc {
    fn from(s: Settings) -> Self {
        SettingsIpc {
            startup_command: s.startup_command,
            working_dir: s.working_dir,
            port: s.port,
            ready_timeout_sec: s.ready_timeout_sec,
            zoom: s.zoom,
            auto_start: s.auto_start,
            keep_alive_on_exit: s.keep_alive_on_exit,
            auto_restart: s.auto_restart,
            terminal_height_ratio: s.terminal_height_ratio,
            use_system_proxy: s.use_system_proxy,
            proxy_url: s.proxy_url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_roundtrip_preserves_all_fields() {
        let original = Settings {
            startup_command: "echo hi".into(),
            working_dir: "D:\\work".into(),
            port: 9090,
            ready_timeout_sec: 60,
            zoom: 1.5,
            auto_start: false,
            keep_alive_on_exit: true,
            auto_restart: true,
            terminal_height_ratio: 0.4,
            use_system_proxy: false,
            proxy_url: "http://127.0.0.1:10808".into(),
        };
        let ipc: SettingsIpc = original.clone().into();
        let back: Settings = ipc.into();
        assert_eq!(back.startup_command, original.startup_command);
        assert_eq!(back.working_dir, original.working_dir);
        assert_eq!(back.port, original.port);
        assert_eq!(back.ready_timeout_sec, original.ready_timeout_sec);
        assert_eq!(back.zoom, original.zoom);
        assert_eq!(back.auto_start, original.auto_start);
        assert_eq!(back.keep_alive_on_exit, original.keep_alive_on_exit);
        assert_eq!(back.auto_restart, original.auto_restart);
        assert_eq!(back.terminal_height_ratio, original.terminal_height_ratio);
        assert_eq!(back.use_system_proxy, original.use_system_proxy);
        assert_eq!(back.proxy_url, original.proxy_url);
    }

    #[test]
    fn ipc_serializes_with_camel_case_field_names() {
        let ipc: SettingsIpc = Settings::default().into();
        let json = serde_json::to_value(&ipc).unwrap();
        assert!(json.get("startupCommand").is_some());
        assert!(json.get("workingDir").is_some());
        assert!(json.get("readyTimeoutSec").is_some());
        assert!(json.get("keepAliveOnExit").is_some());
        assert!(json.get("terminalHeightRatio").is_some());
        assert!(json.get("useSystemProxy").is_some());
        assert!(json.get("proxyUrl").is_some());
        assert!(json.get("startup_command").is_none());
    }

    #[test]
    fn ipc_deserializes_camel_case_from_frontend() {
        let json = serde_json::json!({
            "startupCommand": "npx dsh web",
            "workingDir": "",
            "port": 3080,
            "readyTimeoutSec": 120,
            "zoom": 1.0,
            "autoStart": true,
            "keepAliveOnExit": false,
            "autoRestart": false,
            "terminalHeightRatio": 0.55,
            "useSystemProxy": true,
            "proxyUrl": "",
        });
        let ipc: SettingsIpc = serde_json::from_value(json).unwrap();
        assert_eq!(ipc.startup_command, "npx dsh web");
        assert_eq!(ipc.port, 3080);
        assert!(ipc.use_system_proxy);
        assert_eq!(ipc.proxy_url, "");
    }

    #[test]
    fn old_settings_file_missing_proxy_fields_loads_defaults() {
        // 旧版本 settings.json 没有代理字段：反序列化应落到默认值（serde(default)）
        let json = serde_json::json!({
            "startup_command": "npx dsh web",
            "working_dir": "",
            "port": 3080,
            "ready_timeout_sec": 120,
            "zoom": 1.0,
            "auto_start": true,
            "keep_alive_on_exit": false,
            "auto_restart": false,
            "terminal_height_ratio": 0.55,
        });
        let s: Settings = serde_json::from_value(json).unwrap();
        assert!(s.use_system_proxy, "缺字段应默认 true");
        assert_eq!(s.proxy_url, "");
    }
}
