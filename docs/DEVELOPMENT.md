# 开发文档（dsh-ui）

面向开发者的内容：环境搭建、开发运行、构建打包、调试、架构。用户使用说明见 [README.md](../README.md)，原始设计见 [方案.md](方案.md)。

## 环境要求

- Windows 10/11（系统自带 WebView2）
- Node.js ≥ 20、npm
- Rust 工具链（rustup）+ mingw-w64（GCC）

> 本机无 Visual Studio/MSVC，采用 **GNU 工具链**；如安装 VS Build Tools，可切回 MSVC 工具链（更标准）。

## 从零搭建构建环境

```powershell
# 1. 安装 rustup 与 GNU 工具链（用户级，无需管理员）
rustup-init.exe -y --profile minimal
rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
rustup default stable-x86_64-pc-windows-gnu

# 2. 下载 WinLibs mingw64（含 gcc/windres）并解压，例如：
#    https://github.com/brechtsanders/winlibs_mingw/releases
#    解压到 C:\code\dsh-ui\.tools\mingw64

# 3. 安装前端依赖
cd C:\code\dsh-ui
npm install
```

构建时需保证 `mingw64\bin` 与 `%USERPROFILE%\.cargo\bin` 在 PATH 中。

> 网络受限环境：设置本地代理 `HTTP_PROXY/HTTPS_PROXY`（本机为 `http://127.0.0.1:10808`），
> 并写入 `~/.cargo/config.toml` 与项目 `.npmrc`（均已配置）。

## 开发运行

```bash
npm run dev
```

等价于 `npm run dev:ui`（vite，端口 1420）+ `cargo tauri dev`。

> **注意**：调试版 exe 会把页面 URL 解析到 vite dev server，直接运行
> `target\debug\dsh-ui.exe` 会白屏；调试必须走 `npm run dev`。发布版（release）无此问题。

## 构建安装包

```bash
npm run build
```

产物：`src-tauri\target\release\bundle\nsis\dsh_shell_<版本>_x64-setup.exe`。

### 发布

```powershell
./scripts/publish.ps1             # 完整构建后发布
./scripts/publish.ps1 -SkipBuild  # 仅把最新安装包复制到 release/
```

- 安装包复制到 `<项目根>\release\`（**所有历史版本保留，不清理**，同名版本覆盖）；
- 脚本自动配置 GNU 工具链 PATH 与代理（未设置时默认 `http://127.0.0.1:10808`）。

> **版本号约定**：每次功能改动，版本号**第三段 +1**，前两段保持不变（当前 0.1.25，
> 由 0.1.0 累计 25 次改动而来）。同步修改三处：
> `src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`、`package.json`。
> 安装包命名 `dsh_shell_<版本>_<架构>-setup.exe`（由 `productName: "dsh_shell"` 驱动）。
> 每次发版同步更新 `README.md` 的「更新记录」章节（与版本一一对应）。

> **默认启动命令（0.1.22 起）**：`pnpm dlx @deepseek-ai/dsh@next web --no-open`——pnpm 替代 npx
> （`npm i -g pnpm`）；`@next` 是 npm dist-tag（当前 0.1.0-rc.8，最新预发布线）；
> `--no-open` 阻止新版 dsh（rc.7+）自动打开默认浏览器（见
> `packages/bundle/web-app/src/startup.ts` 的 web flag 家族；rc.6 及更早不支持该参数）。
> pnpm 10+ 默认禁止依赖 postinstall 脚本，需 `pnpm config set dangerouslyAllowAllBuilds true`。

NSIS 使用自定义模板 `src-tauri\nsis\installer.nsi`（基于 tauri 2.11.4 官方模板改造）：

- 增加「标准安装 / 绿色安装」选择页（绿色安装不写注册表、不建快捷方式）；
- 绿色安装**跳过运行状态检测**（`CheckIfAppIsRunning` 仅标准模式执行），只解压；
- 绿色安装写入 `portable.marker` 标记，壳据此把配置存到程序目录（配置随目录走）；
- 静默参数：`/S`、`/G`（绿色）、`/D=<路径>`；
- `tauri.conf.json` 的 `nsis.languages` 限为 `SimpChinese` + `English`（自定义 LangString 只定义了这两种）。

### 打包相关坑（已踩）

1. **WebView2Loader.dll 漏打包**：bundler 对 gnu 工具链的自动打包未生效，必须在
   `tauri.conf.json` 的 `bundle.resources` 显式配置：
   `"resources": { "target/release/WebView2Loader.dll": "WebView2Loader.dll" }`。
2. **README.md 打进安装包**：同样经 `bundle.resources`：
   `"../README.md": "README.md"`（key 相对 `src-tauri/`，value 为安装目录内文件名），
   标准/绿色安装都会带上；发版时记得同步更新。
2. **换图标后 exe 资源不更新**：tauri-build 未把图标加入 `rerun-if-changed`，
   `src-tauri/build.rs` 已显式声明 `cargo:rerun-if-changed=icons/icon.ico`；
   改图标后如未生效，touch 一下 `tauri.conf.json` 强制重跑。
3. **GNU 链接警告** `.rsrc merge failure: multiple non-default manifests`：无害，可忽略；
   换成 MSVC 工具链后消失。

## 调试

设置环境变量后启动：

```powershell
$env:DSH_DEBUG_DEVTOOLS = '1'
npm run dev
```

- 自动打开主窗口的 DevTools；
- 主窗口启用 `--remote-debugging-port=9222`，可用 Chrome DevTools 协议检查/驱动
  webview（`.tools\cdp-*.mjs` 有现成脚本：查 DOM 状态、模拟点击、读终端缓冲、
  触发下载验证等）。

> **iframe 是独立 CDP target（OOPIF）**：DSH UI 与 shell 页跨源，WebView2 将 iframe
> 放入独立进程，`/json` 里会出现 `type: "iframe"` 的 target——可直接连接它在
> 127.0.0.1:3080 同源上下文执行 JS（如触发下载、读 harness UI 状态）。

### 下载支持说明

`src-tauri/src/download.rs` 通过 `Webview::with_webview` 拿 PlatformWebview →
`ICoreWebView2_4` 挂 `DownloadStarting`、`ICoreWebView2_8` 挂 `PermissionRequested`：

- **PermissionRequested**：自动允许 `MULTIPLE_AUTOMATIC_DOWNLOADS`（否则 WebView2 弹
  `edge://permission-request-dialog`，下载卡住）；
- **DownloadStarting**：解析文件名（Content-Disposition → query 的 sessionId 兜底），
  保存到 `%USERPROFILE%\Downloads`，`SetResultFilePath + SetHandled`；
- **StateChanged**：下载终态 → 前端 `download:completed` 事件（面板日志）。

> 背景：wry 默认下载 handler 只放行不设路径（`|_, _| true`），WebView2 无内置
> "另存为" UI，路径为空时下载被取消——这就是"点击导出无反应"的根因。
> 另注意：shell 页（tauri.localhost）与 3080 跨源，`<a download>` 跨源无效会变成
> 顶层导航；验证/触发下载必须在 iframe 的 OOPIF target 里执行。

### 缩放注入说明

~~注入脚本方案已废弃~~（多窗口/注入在 Windows 上不可靠）。缩放由 shell 页面直接调用
`zoom_step` / `zoom_set` 命令实现（按钮 + `Ctrl+=/-/0`，shell 页面焦点时有效；
iframe 内快捷键不可注入——跨域限制，缩放按钮不受影响）。

### 已知问题

- **Windows 第二个 WebView2 窗口合成不可显示**：内容渲染正常（PrintWindow 可见）
  但屏幕不显示；DevTools 打开时同样不显示。因此采用单窗口 + iframe 架构。
- **GNU 链接警告** `.rsrc merge failure: multiple non-default manifests`：无害，可忽略。
- **`cargo test` 在 Windows GNU 工具链下无法运行**（`0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND`，
  加载器阶段挂起）：tauri/webview2 依赖与 test harness 的已知问题
  （[tauri#11028](https://github.com/tauri-apps/tauri/issues/11028)）。单元测试代码已保留
  （`#[cfg(test)]`），可在 MSVC/CI 环境运行；本机以等价逻辑脚本 + 端到端验证替代
  （`.tools/verify-filename.mjs`、CDP 设置读写验证）。
- **`std::env::set_var/remove_var` 多线程调用**（`server.rs::apply_proxy_env`）：进程全局
  环境变量非同步；edition 2021 下合法可编译（Rust 2024 起为 unsafe）。仅在启动会话/退出
  keep-alive 前调用一次，风险低，属已知 footgun。

## 架构速览

```
src-tauri/src/
├─ main.rs / lib.rs     # 入口；主窗口创建（WebviewWindowBuilder）、事件监听、IPC 注册
├─ server.rs            # 生命周期状态机：boot/ready/failed/stopped、就绪探测、keep-alive、
│                       # 重启/手动停止（按端口杀 detached）、退出清理（keep-alive 脱离 +
│                       # 关闭时询问是否结束后台服务）、最后会话 workspace 跟随、
│                       # 首次运行检测（dsh_available → 下载提示 + 超时放宽）、
│                       # 代理 env 应用（系统代理/自定义，供 npx 下载）
├─ terminal.rs          # ConPTY 会话（portable-pty）：cmd.exe + 进程树 Job Object
├─ zoom.rs              # 缩放（50–300%）：应用/持久化
├─ download.rs          # WebView2 下载支持（下载起始/权限/完成事件）
├─ job.rs               # Windows Job Object（KILL_ON_JOB_CLOSE 退出清树）
├─ settings.rs          # settings.json 读写
└─ commands.rs          # IPC 命令层（get_state / terminal_* / zoom_* / restart / stop / settings）

ui/                     # 前端（Vite + 原生 TS + xterm.js），单页面
└─ index.html / shell.ts # 主窗口 shell：iframe 内嵌 DSH UI + 启动遮罩（DeepSeek logo +
                          # 内嵌只读 xterm 实时显示启动输出）+ 右下角终端小按钮
                          # （点+终端，无地址）+ 终端面板（xterm）+ 设置弹窗
```

关键行为：

- **单窗口架构**：主窗口 webview 加载 shell 页面，DSH UI 通过 iframe 加载
  （`http://127.0.0.1:3080/`，该服务无 X-Frame-Options 限制）；右下角固定按钮
  点击呼出/隐藏终端面板。**不创建第二个窗口**——Windows 上第二个 WebView2
  窗口的合成不可靠（内容渲染但不显示，原因未明；DevTools 打开时更甚）。
- **便携模式**：程序目录存在 `portable.marker` 时，`settings.json` 与后台服务日志
  存到程序同目录（绿色安装），否则 `%APPDATA%\com.dsh-ui.app`。
- **工作目录跟随**：`working_dir` 留空时，启动扫描 `~/.dsh/sessions/*/` 取最新会话
  的 workspace（projectKey 有损编码 → 分段合并枚举候选 + 目录存在性验证还原），
  作为 dsh web 的启动目录；手动设置后不覆盖。
- **就绪判定**：端口 TCP 有监听即「直连」（不区分占用者）；启动路径以 HTTP 200 为准，
  终端输出扫失败特征（`eaddrinuse` 等）快速失败；
- **缩放**：WebView2 zoom factor 由 shell 页面按钮/Ctrl±0 触发（iframe 内快捷键
  不可注入，缩放按钮不受影响）；
- **退出清理**：应用退出时 Job Object 句柄关闭 → cmd/npx/node 整树终止；
  `keep_alive_on_exit` 时关闭会弹询问框（默认「保持运行」继续启动脱离进程，下次启动直连；
  「结束服务」则按端口杀 detached）；运行中也可用终端面板「⏹」手动停止——先杀当前会话树，
  端口仍被监听（detached/外部进程）时兜底 `netstat -ano` 找 PID → `taskkill /T /F`
  （解析逻辑 `parse_listening_pids` 有单测，node 等价验证 `.tools/verify-stop-service.mjs`）；
- **单实例**：`tauri-plugin-single-instance`，双开只唤起已有实例。
