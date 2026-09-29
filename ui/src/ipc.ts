import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** 生命周期阶段 */
export type Phase = "boot" | "ready" | "failed" | "stopped";

export interface AppState {
  phase: Phase;
  message?: string;
  url: string;
  zoom: number;
}

export interface Settings {
  startupCommand: string;
  workingDir: string;
  port: number;
  readyTimeoutSec: number;
  zoom: number;
  autoStart: boolean;
  keepAliveOnExit: boolean;
  autoRestart: boolean;
  terminalHeightRatio: number;
  useSystemProxy: boolean;
  proxyUrl: string;
}

export const DEFAULT_SETTINGS: Readonly<Settings> = {
  startupCommand: "pnpm dlx @deepseek-ai/dsh@next web --no-open",
  workingDir: "",
  port: 3080,
  readyTimeoutSec: 120,
  zoom: 1.0,
  autoStart: true,
  keepAliveOnExit: false,
  autoRestart: false,
  terminalHeightRatio: 0.55,
  useSystemProxy: true,
  proxyUrl: "",
};

// ---------- 命令 ----------
export const cmd = {
  getState: () => invoke<AppState>("get_state"),
  terminalInput: (data: string) => invoke<void>("terminal_input", { data }),
  terminalResize: (cols: number, rows: number) =>
    invoke<void>("terminal_resize", { cols, rows }),
  getTerminalBuffer: () => invoke<string>("get_terminal_buffer"),
  zoomStep: (delta: number) => invoke<number>("zoom_step", { delta }),
  zoomSet: (factor: number) => invoke<number>("zoom_set", { factor }),
  restartService: () => invoke<void>("restart_service"),
  stopService: () => invoke<void>("stop_service"),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (s: Settings) => invoke<void>("save_settings", { s }),
  openBrowser: (url: string) => invoke<void>("open_browser", { url }),
};

// ---------- 事件订阅（listen 常驻回调，返回取消函数） ----------
/** state:changed 载荷与 AppState 同构（后端同一份 JSON），复用单一类型防漂移 */
export type StateChangedPayload = AppState;

export interface TerminalDataPayload {
  data: string;
}

export const onStateChanged = (cb: (p: StateChangedPayload) => void) =>
  listen<StateChangedPayload>("state:changed", (e) => cb(e.payload));

export const onTerminalData = (cb: (p: TerminalDataPayload) => void) =>
  listen<TerminalDataPayload>("terminal:data", (e) => cb(e.payload));

export const onTerminalExit = (cb: (p: { code?: number }) => void) =>
  listen<{ code?: number }>("terminal:exit", (e) => cb(e.payload));

export const onDownloadStarting = (cb: (p: { path: string; name: string }) => void) =>
  listen<{ path: string; name: string }>("download:starting", (e) => cb(e.payload));

export const onDownloadCompleted = (cb: (p: { path: string; ok: boolean }) => void) =>
  listen<{ path: string; ok: boolean }>("download:completed", (e) => cb(e.payload));

export const onKeepAliveFailed = (cb: (p: { error: string }) => void) =>
  listen<{ error: string }>("keepalive:failed", (e) => cb(e.payload));

/** 直连场景：服务要求浏览器认证（壳未持有 token/cookie），需引导用户重启服务完成登录 */
export const onAuthRequired = (cb: () => void) => listen("auth:required", () => cb());
