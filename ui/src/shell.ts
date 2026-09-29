import { Terminal } from "xterm";
import { FitAddon } from "xterm-addon-fit";
import "xterm/css/xterm.css";
import {
  cmd,
  onStateChanged,
  onTerminalData,
  onTerminalExit,
  onDownloadStarting,
  onDownloadCompleted,
  onKeepAliveFailed,
  onAuthRequired,
  DEFAULT_SETTINGS,
  type Settings,
} from "./ipc";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

// ---------- DOM ----------
const frame = document.getElementById("app-frame") as HTMLIFrameElement;
const bootMask = document.getElementById("boot-mask")!;
const phaseText = document.getElementById("phase-text")!;
const spinner = document.getElementById("spinner")!;
const errorBox = document.getElementById("error-box")!;
const errorText = document.getElementById("error-text")!;
const floatBtn = document.getElementById("float-btn")!;
const floatDot = document.getElementById("float-dot")!;
const panel = document.getElementById("panel")!;
const stateChip = document.getElementById("panel-state")!;
const zoomLabel = document.getElementById("btn-zoom-label") as HTMLButtonElement;
const logEl = document.getElementById("panel-log")!;
const browserBtn = document.getElementById("btn-open-browser") as HTMLButtonElement;

// ---------- 终端 ----------
const term = new Terminal({
  fontFamily: "Cascadia Mono, Consolas, 'Courier New', monospace",
  fontSize: 13,
  cursorBlink: true,
  scrollback: 5000,
  theme: {
    background: "#0d1117",
    foreground: "#c9d1d9",
    cursor: "#58a6ff",
  },
});
const fit = new FitAddon();
term.loadAddon(fit);
term.open(document.getElementById("terminal")!);

term.onData((data) => {
  cmd.terminalInput(data).catch((e) => console.error("终端输入失败:", e));
});
term.onResize(({ cols, rows }) => void cmd.terminalResize(cols, rows));

const terminalEl = document.getElementById("terminal")!;
const ro = new ResizeObserver(() => {
  try {
    fit.fit();
  } catch {
    /* 面板隐藏时无法 fit */
  }
});
ro.observe(terminalEl);
setTimeout(() => fit.fit(), 150);

// 启动遮罩上的终端：boot/failed/stopped 阶段同步显示 console 输出，
// 支持直接输入（首次运行/安装过程的交互确认可直接在此应答）
const bootTerm = new Terminal({
  fontFamily: "Cascadia Mono, Consolas, 'Courier New', monospace",
  fontSize: 12,
  cursorBlink: true,
  scrollback: 2000,
  theme: {
    background: "#0d1117",
    foreground: "#c9d1d9",
    cursor: "#58a6ff",
  },
});
const bootFit = new FitAddon();
bootTerm.loadAddon(bootFit);
bootTerm.open(document.getElementById("boot-terminal")!);
// 与面板终端同一条输入链路（terminal_input → ConPTY 会话）
bootTerm.onData((data) => {
  cmd.terminalInput(data).catch((e) => console.error("终端输入失败:", e));
});

const bootTermEl = document.getElementById("boot-terminal")!;
const roBoot = new ResizeObserver(() => {
  try {
    bootFit.fit();
  } catch {
    /* 遮罩隐藏时无法 fit */
  }
});
roBoot.observe(bootTermEl);

/** 遮罩可见（boot/failed/stopped）时重排终端尺寸 */
function fitBootTerm() {
  setTimeout(() => {
    try {
      bootFit.fit();
    } catch {
      /* 遮罩隐藏时无法 fit */
    }
  }, 60);
}

// ---------- 状态 ----------
let url = "http://127.0.0.1:3080/";
let loaded = false;
// 上一次相位：用于识别"进入 ready"的转换（每次都（重新）加载 iframe），
// 而不是依赖 URL 变化——改端口保存时 URL 已变但服务未起，按 URL 比较会提前导航到空端口
let lastPhase: string | null = null;
// 启动 watchdog：轮询 getState 连续失败（IPC 层不可用）时给出可操作错误
let ipcFailures = 0;
const IPC_FAIL_LIMIT = 3;

function showIpcError() {
  spinner.hidden = true;
  errorBox.hidden = false;
  browserBtn.hidden = true;
  phaseText.textContent = "界面通信失败";
  errorText.textContent = "无法与主进程通信。请关闭窗口后重新打开应用。";
  // IPC 已不可用：重试按钮只会产生控制台 rejection，禁用并说明
  const retry = document.getElementById("btn-retry") as HTMLButtonElement | null;
  if (retry) {
    retry.disabled = true;
    retry.title = "主进程通信已中断";
  }
}

function applyState(p: { phase: string; message?: string; url: string; zoom: number }) {
  url = p.url;
  zoomLabel.textContent = `${Math.round(p.zoom * 100)}%`;

  floatDot.classList.remove("ok", "err", "boot", "stop");
  stateChip.classList.remove("ok", "err", "boot", "stop");

  // 统一复位（幂等：任意相邻状态切换都不残留前一个 phase 的元素状态）
  errorBox.hidden = true;
  spinner.hidden = true;
  browserBtn.hidden = true;

  if (p.phase === "ready") {
    floatDot.classList.add("ok");
    stateChip.textContent = "运行中";
    stateChip.classList.add("ok");
    bootMask.hidden = true;
    floatBtn.hidden = false;
    // IPC 恢复可用：解除 watchdog 的禁用
    const retry = document.getElementById("btn-retry") as HTMLButtonElement | null;
    if (retry) {
      retry.disabled = false;
      retry.title = "重新启动服务";
    }
    // 进入 ready（首次启动 / 重启后就绪 / 从失败恢复）一律加载或重载 iframe：
    // 覆盖"改端口保存时提前导航到空端口留下的错误页"这类无法自愈的状态。
    // 运行中（ready→ready）仅 URL 变化不在此导航——服务尚未就绪，交给下一次相位转换。
    const enteredReady = lastPhase !== "ready";
    if (!loaded || enteredReady) {
      loaded = true;
      frame.src = p.url;
    }
  } else if (p.phase === "failed") {
    floatDot.classList.add("err");
    stateChip.textContent = "已停止";
    stateChip.classList.add("err");
    bootMask.hidden = false;
    floatBtn.hidden = false;
    errorBox.hidden = false;
    browserBtn.hidden = false; // failed 状态下可"在浏览器中打开"排查
    phaseText.textContent = "启动失败";
    errorText.textContent = p.message || "";
    fitBootTerm();
  } else if (p.phase === "stopped") {
    floatDot.classList.add("stop");
    stateChip.textContent = "已停止";
    stateChip.classList.add("stop");
    bootMask.hidden = false;
    floatBtn.hidden = false;
    errorBox.hidden = false;
    phaseText.textContent = "服务已停止";
    errorText.textContent = p.message || "服务已手动停止";
    fitBootTerm();
  } else {
    floatDot.classList.add("boot");
    stateChip.textContent = "启动中";
    stateChip.classList.add("boot");
    bootMask.hidden = false;
    floatBtn.hidden = false;
    spinner.hidden = false;
    phaseText.textContent = p.message || "正在启动服务…";
    fitBootTerm();
  }
  lastPhase = p.phase;
}

async function refreshState() {
  try {
    ipcFailures = 0;
    applyState(await cmd.getState());
  } catch (e) {
    ipcFailures += 1;
    console.error("读取状态失败:", e);
    // IPC 连续失败：模块/桥接损坏时给出可操作错误而非无限 spinner
    if (ipcFailures >= IPC_FAIL_LIMIT && bootMask.hidden === false) {
      showIpcError();
    }
  }
}

// 事件驱动 + 2s 轮询兜底
void onStateChanged((p) => applyState(p));
void refreshState();
setInterval(refreshState, 2000);

// ---------- 终端输出 ----------
// 面板终端与启动遮罩终端同步写入。先订阅实时输出到 pending 缓冲，
// 再补发历史快照，最后按序 flush——快照与订阅之间的输出不丢失（防竞态）
void (async () => {
  const pending: string[] = [];
  let live = false;
  void onTerminalData((p) => {
    if (live) {
      term.write(p.data);
      bootTerm.write(p.data);
    } else {
      pending.push(p.data);
    }
  });
  try {
    const snap = await cmd.getTerminalBuffer();
    if (snap) {
      term.write(snap);
      bootTerm.write(snap);
    }
  } catch (e) {
    console.error("读取终端缓冲失败:", e);
  }
  for (const d of pending) {
    term.write(d);
    bootTerm.write(d);
  }
  pending.length = 0;
  live = true;
})();

const LOG_MAX_LINES = 200;

function log(msg: string) {
  const line = document.createElement("div");
  line.className = "log-line";
  line.textContent = `[${new Date().toLocaleTimeString()}] ${msg}`;
  logEl.appendChild(line);
  // 长会话下日志 DOM 不无限增长：超出上限丢弃最旧行
  while (logEl.childElementCount > LOG_MAX_LINES) {
    logEl.firstElementChild?.remove();
  }
  logEl.scrollTop = logEl.scrollHeight;
}

// ---------- 直连场景：服务要求浏览器认证（无 token 可捕获） ----------
let bannerTimer: number | undefined;
function showBanner(text: string) {
  let banner = document.getElementById("auth-banner");
  if (!banner) {
    banner = document.createElement("div");
    banner.id = "auth-banner";
    banner.className = "banner";
    document.body.appendChild(banner);
  }
  banner.textContent = text;
  banner.hidden = false;
  if (bannerTimer !== undefined) window.clearTimeout(bannerTimer);
  bannerTimer = window.setTimeout(() => {
    banner!.hidden = true;
  }, 8000);
}

void onAuthRequired(() => {
  const text = "服务需要浏览器认证：请点终端面板「⟳ 重新启动服务」完成登录";
  log(text);
  showBanner(text);
});

void onTerminalExit((p) => {
  log(`终端进程已退出${p.code !== undefined ? `（退出码 ${p.code}）` : ""}`);
});

// ---------- 下载（session log 导出等） ----------
void onDownloadStarting((p) => {
  log(`开始下载：${p.name} → ${p.path}`);
});
void onDownloadCompleted((p) => {
  log(p.ok ? `下载完成：${p.path}` : `下载失败：${p.path || "未知错误"}`);
});
void onKeepAliveFailed((p) => {
  log(`后台服务启动失败：${p.error}`);
});

// ---------- 按钮：呼出/隐藏终端面板 ----------
// 面板高度按设置比例（terminalHeightRatio，CSS 55% 兜底）
void cmd
  .getSettings()
  .then((s) => {
    panel.style.height = `${s.terminalHeightRatio * 100}%`;
  })
  .catch((e) => console.error("读取设置失败:", e));

function togglePanel() {
  const show = panel.hidden;
  panel.hidden = !show;
  if (show) {
    setTimeout(() => fit.fit(), 60);
  }
}

floatBtn.addEventListener("click", togglePanel);
document.getElementById("btn-hide-panel")!.addEventListener("click", togglePanel);

// ---------- 工具栏：缩放 / 重启 ----------
document.getElementById("btn-zoom-out")!.addEventListener("click", () => {
  void cmd.zoomStep(-1).then((z) => (zoomLabel.textContent = `${Math.round(z * 100)}%`));
});
document.getElementById("btn-zoom-label")!.addEventListener("click", () => {
  void cmd.zoomSet(1.0).then((z) => (zoomLabel.textContent = `${Math.round(z * 100)}%`));
});
document.getElementById("btn-zoom-in")!.addEventListener("click", () => {
  void cmd.zoomStep(1).then((z) => (zoomLabel.textContent = `${Math.round(z * 100)}%`));
});
document.getElementById("btn-restart")!.addEventListener("click", () => void cmd.restartService());
document.getElementById("btn-stop")!.addEventListener("click", async () => {
  try {
    await cmd.stopService();
    log("已请求停止服务");
  } catch (e) {
    console.error("停止服务失败:", e);
    log(`停止服务失败：${String(e)}`);
  }
});

// 快捷键：Ctrl+=/-/0 缩放（本地 shell 页焦点时）
window.addEventListener("keydown", (e) => {
  if (!e.ctrlKey && !e.metaKey) return;
  const k = e.key.toLowerCase();
  if (k === "=" || k === "+") {
    e.preventDefault();
    void cmd.zoomStep(1).then((z) => (zoomLabel.textContent = `${Math.round(z * 100)}%`));
  } else if (k === "-") {
    e.preventDefault();
    void cmd.zoomStep(-1).then((z) => (zoomLabel.textContent = `${Math.round(z * 100)}%`));
  } else if (k === "0") {
    e.preventDefault();
    void cmd.zoomSet(1.0).then((z) => (zoomLabel.textContent = `${Math.round(z * 100)}%`));
  }
});

// ---------- 启动失败：重试 / 浏览器打开 ----------
document.getElementById("btn-retry")!.addEventListener("click", () => void cmd.restartService());
document.getElementById("btn-open-browser")!.addEventListener("click", () => cmd.openBrowser(url));

// ---------- 设置 ----------
const settingsModal = document.getElementById("settings-modal")!;
const cfgCommand = document.getElementById("cfg-command") as HTMLInputElement;
const cfgWorkdir = document.getElementById("cfg-workdir") as HTMLInputElement;
const cfgPort = document.getElementById("cfg-port") as HTMLInputElement;
const cfgTimeout = document.getElementById("cfg-timeout") as HTMLInputElement;
const cfgKeepalive = document.getElementById("cfg-keepalive") as HTMLInputElement;
const cfgAutorestart = document.getElementById("cfg-autorestart") as HTMLInputElement;
const cfgSysProxy = document.getElementById("cfg-sysproxy") as HTMLInputElement;
const cfgProxy = document.getElementById("cfg-proxy") as HTMLInputElement;

// 勾选"使用系统代理"时禁用自定义代理输入框
cfgSysProxy.addEventListener("change", () => {
  cfgProxy.disabled = cfgSysProxy.checked;
});

// 打开设置时记录当前设置：保存时只合并弹窗内编辑的字段，
// zoom / autoStart / terminalHeightRatio 等未暴露字段沿用磁盘值（后端亦保留）
let lastSettings: Settings | null = null;

document.getElementById("btn-settings")!.addEventListener("click", async () => {
  let s: Settings;
  try {
    s = await cmd.getSettings();
  } catch (e) {
    console.error("读取设置失败:", e);
    s = { ...DEFAULT_SETTINGS };
  }
  lastSettings = s;
  cfgCommand.value = s.startupCommand;
  cfgWorkdir.value = s.workingDir;
  cfgPort.value = String(s.port);
  cfgTimeout.value = String(s.readyTimeoutSec);
  cfgKeepalive.checked = s.keepAliveOnExit;
  cfgAutorestart.checked = s.autoRestart;
  cfgSysProxy.checked = s.useSystemProxy;
  cfgProxy.value = s.proxyUrl;
  cfgProxy.disabled = s.useSystemProxy;
  settingsModal.hidden = false;
});

const closeSettings = () => {
  settingsModal.hidden = true;
};
document.getElementById("btn-close-settings")!.addEventListener("click", closeSettings);
document.getElementById("btn-cancel-settings")!.addEventListener("click", closeSettings);

// 保存提示（2 秒后自动消失）
let saveTipTimer: number | undefined;
function showSaveTip(text: string) {
  let tip = document.getElementById("save-tip");
  if (!tip) {
    tip = document.createElement("div");
    tip.id = "save-tip";
    tip.className = "save-tip";
    document.getElementById("settings-modal")!.appendChild(tip);
  }
  tip.textContent = text;
  tip.hidden = false;
  if (saveTipTimer !== undefined) window.clearTimeout(saveTipTimer);
  saveTipTimer = window.setTimeout(() => {
    tip!.hidden = true;
  }, 2500);
}

document.getElementById("btn-save-settings")!.addEventListener("click", async () => {
  const base = lastSettings ?? { ...DEFAULT_SETTINGS };
  // 仅覆盖弹窗编辑的字段；数值整数化（step=1 之外再防 3080.5 / 1e2 类输入）
  const clampInt = (v: number, min: number, max: number, fb: number) =>
    Math.trunc(Math.max(min, Math.min(max, Number.isFinite(v) ? v : fb)));
  const s: Settings = {
    ...base,
    startupCommand: cfgCommand.value.trim() || DEFAULT_SETTINGS.startupCommand,
    workingDir: cfgWorkdir.value.trim(),
    port: clampInt(Number(cfgPort.value), 1, 65535, 3080),
    readyTimeoutSec: clampInt(Number(cfgTimeout.value), 10, 600, 120),
    keepAliveOnExit: cfgKeepalive.checked,
    autoRestart: cfgAutorestart.checked,
    useSystemProxy: cfgSysProxy.checked,
    // 系统代理开启时代理地址字段不生效，沿用磁盘值（保留手动模式备用地址）
    proxyUrl: cfgSysProxy.checked ? base.proxyUrl : cfgProxy.value.trim(),
  };
  try {
    await cmd.saveSettings(s);
    log("设置已保存：重启服务后生效");
    showSaveTip("设置已保存，重启服务后生效");
  } catch (e) {
    console.error("保存设置失败:", e);
    showSaveTip(`保存失败：${String(e)}`);
  }
});

document.getElementById("btn-pick-dir")!.addEventListener("click", async () => {
  const dir = await openDialog({ directory: true, multiple: false });
  if (typeof dir === "string") cfgWorkdir.value = dir;
});
