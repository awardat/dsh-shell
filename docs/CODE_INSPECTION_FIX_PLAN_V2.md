# dsh-ui CODE_INSPECTION_REPORT_V2 整改方案

> 状态：✅ 已执行（**0.1.29**，2026 年）。全部 P1/P2/P3（除 P3-10 观察项）落地并有 e2e 佐证。
> 来源：[CODE_INSPECTION_REPORT_V2.md](CODE_INSPECTION_REPORT_V2.md)（第 4 轮检查，基准 0.1.28）。
> 全部发现均已对照源码逐条核实，结论见下。
>
> **执行期新增发现（P2-1 深化）**：按本方案改为「累计滑窗匹配」后，用分片 dummy 实测**仍然漏捕获**。
> 加临时诊断打印原始读块取证，发现 ConPTY 的真实形态比预想更碎：token 行被拆成
> **三片**，且**子进程日志行插在 URL 中间**（`dsh web: http://127.0.0.1:` + `svc listening on 3999 (token=…)`
> + `3999/?token=…`）。任何「连续/去空白」匹配都不可能命中，故最终实现为**三级回退**：
> ① 连续匹配 → ② 去换行后匹配 → ③ **要素重组**（取 `dsh web:` 之后的 loopback 端口 + token 值，
> 仅在确认该行含 loopback 字面时启用，避免把 `(LAN: …)` 误重组为本机地址）；
> 端口解析按「连续前缀 → `<port>/?token=` → 首个 3–5 位数字串（排除 IP 段，避免把 `127.0.0.1` 的 `127` 当端口）」
> 三级取用。回归脚本：`.tools/verify-extract-loose.mjs`（含取证形态）、`.tools/auth-dummy-split.mjs`。

---

## 一、核实结论

| 编号 | 核实 | 依据 |
|---|---|---|
| P1-1 改端口后 iframe 残留错误页 | ✅ 属实 | `commands.rs:110` 保存后无条件 `emit_state`（相位仍 ready、URL 已新）→ `shell.ts:151` 的 URL 比较成立 → 保存瞬间导航到**尚无服务**的新端口 → 错误页；重启后 `frame.src` 已等于新 URL，条件不再成立 → 不再赋值；`loaded`（`shell.ts:108`）全程不复位 → 仅重启客户端可自救。**0.1.24 修复引入的次生缺陷**，当时 e2e 因目标端口预先有 dummy 监听而掩盖 |
| P2-1 token 行跨读块断裂漏捕获 | ✅ 属实 | `server.rs:741` 对单次 8 KB 读块 `extract_dsh_web_url(&text)`；分片即漏 → `auth_pending` 不置位 → 探针宽限后按「旧版无需认证」Ready → 401。另：`server.rs:746` 的 `auth_pending` 置位**无 gen 校验**（`auth_done` 回调有） |
| P2-2 直连 401 无恢复路径 + 陈旧注释 | ✅ 属实 | 直连（connect-only）无终端输出 → 永不捕获 token；cookie 缺失/过期时 iframe 401 且界面无引导。`shell.ts:140` 注释所述「`?_=…` 缓存破拆重载」在产品代码中不存在（仅 `.tools/cdp-reload-test.mjs` 实验残留） |
| P3-1 zoom `step` 失败不回滚 | ✅ 属实 | `zoom.rs:70-76`：先写内存态、后 `set_zoom`，失败时 `?` 提前返回，内存=新/窗口=旧 |
| P3-2 `term_buffer` append 无 gen 校验 | ✅ 属实 | `server.rs:731-736` 未比对 gen（同段 `terminal:data` emit 亦无）→ 陈旧 reader 残余输出可混入新会话快照 |
| P3-3 `download_seen` 两段锁 TOCTOU | ✅ 属实 | `server.rs:749-761`：先锁查 gen、再锁写 `needs_download`/`ready_timeout` |
| P3-4 `dsh_available()` 直连分支白跑 | ✅ 属实 | `server.rs:603` 早于 `607 port_listening` 判断；直连分支弃用其结果（`where` 子进程 + 两处目录扫描） |
| P3-5 `blocking_show` 主线程注释无依据 | ✅ 属实 | 注释称「无死锁风险」但未说明前提 |
| P3-6 README 安装包名滞后（第 3 次） | ✅ 属实 | `README.md:28` 仍写 `0.1.24` |
| P3-7 DEVELOPMENT 架构节滞后 | ✅ 属实 | 模块树缺 `auth.rs`；就绪判定仍写「HTTP 200」；下载说明未提来源校验；`server.rs` 条目残留「供 npx 下载」 |
| P3-8 IPC 失败后重试按钮仍可点 | ✅ 属实 | `shell.ts:113-119 showIpcError` 后 `btn-retry` 仍触发 `restartService`（仅控制台 rejection） |
| P3-9 `log()` 无行数上限 | ✅ 属实 | `shell.ts` 日志 DOM 无限增长 |
| P3-10 `csp: null` | 部分采纳 | 导航已收窄、下载已校验来源、内容为本机可信服务——**维持现状**，作为观察项在 DEVELOPMENT 记录，不改配置 |

---

## 二、整改设计

### P1-1 改为「相位进入 ready 时重载」

**根因**：既有实现用「URL 是否变化」决定是否加载，混淆了两种语义——① *首次就绪/重启后就绪应重新加载*；② *运行中 URL 变化不应提前导航*。

**改法**（`ui/src/shell.ts`）：
```ts
let lastPhase: string | null = null;
// ready 分支内：
const enteredReady = lastPhase !== "ready";
if (!loaded || enteredReady) {          // 进入 ready 一律（重新）加载
  loaded = true;
  frame.src = p.url;
}
// 函数末尾：lastPhase = p.phase;
```
- 保存改端口（ready→ready）：**不再导航**到尚无服务的新端口 ✓（消除"保存即提前跳转"）
- 点 ⟳ 重启（boot→ready）：`enteredReady` 为真 → 无条件赋值 → 覆盖残留错误页 ✓
- 同端口重启：同样重载（服务已重启，页面重连属预期行为）
- 删除已无用的 `norm()` 与 `shell.ts:140` 陈旧注释（`?_=…` 机制不存在）

**验证（新增 e2e，内容级断言）**：
1. 端口 3999 + dummy 3999：就绪 → 保存改 `port=4000`（命令仍 3999）→ 断言 iframe **未被导航**（OOPIF target URL 仍 3999、内容正常）
2. 再把命令改为 `node dummy-server.mjs 4000` → 点 ⟳ → Boot → Ready → 断言 OOPIF **DOM 内容**（页面标题/正文）为 4000 的新页面，而非错误页

### P2-1 token 捕获改累计滑窗 + gen 校验

**改法**（`server.rs` reader 线程）：
- 把 token 提取移到 `recent.push_str(&text)` + `trim_window` **之后**，用 `recent`（4 KB 滑窗）匹配 → 跨块拼接的行也能命中；`auth_exchange_started` 仍防重复与重复扫描
- `auth_pending` 置位改为**单锁内 gen 校验后**置位（与 `auth_done` 对称）

**验证**：新增 `.tools/auth-dummy-split.mjs`（分两次 flush 输出 token 行，如先 `dsh web: http://127.0.0.1:3999/?tok` 再 `en=ABC`）→ 壳启动 → 断言 cookie 注入成功且 iframe 内容为 200 页

### P2-2 直连 401 给出恢复引导（不自动重启）

**改法**：
- `server.rs`：`http_responsive` 扩展为返回状态码的 `http_status(port) -> Option<u16>`（`http_responsive` 复用之）；直连分支探测到 **401** 时，终端写一行提示 + `emit("auth:required")`
- 前端 `ipc.ts` 增 `onAuthRequired`；`shell.ts` 显示**顶部提示条**（新增 `.banner` 样式，约 8 秒）：「服务需要浏览器认证：请点终端面板「⟳ 重新启动服务」完成登录」，同时写入面板日志
- **不自动 stop+restart**：会打断用户可能正在使用的服务（3080 主服务场景），交用户决定
- 修正 `shell.ts:140` 陈旧注释（并入 P1-1）

**验证**：新增 e2e——外部先起 `auth-dummy`（401 门）→ 壳直连 → 断言提示条出现且文案正确

### P3 逐项

| # | 改法 |
|---|---|
| P3-1 | `zoom.rs::step`：`set_zoom` 失败分支同样回滚内存态（`g.zoom`/`g.settings.zoom`）并返回 Err |
| P3-2 | `server.rs` reader：`term_buffer` append 前复查 gen（不一致则不追加）；`terminal:data` emit 同样加 gen 判定 |
| P3-3 | `download_seen` 命中改为**单锁复合**：锁内读 gen + 置 `download_seen`/`needs_download`/`ready_timeout` |
| P3-4 | `boot()` 把 `dsh_available()` 移入端口空闲分支（直连分支不再白跑子进程/目录扫描） |
| P3-5 | `ask_keep_alive` 注释补依据：tauri-plugin-dialog 的 `blocking_show` 经 `sync_channel` + 独立线程运行 rfd `AsyncMessageDialog`，Windows 原生模态对话框自带消息泵；实测可用，但属依赖实现细节 |
| P3-6 | **机制化**：README「从 0 开始」不再硬编码版本号，改为「获取最新安装包（`release/` 目录或 GitHub Releases，文件名 `dsh_shell_<版本>_x64-setup.exe`）」 |
| P3-7 | DEVELOPMENT 更新：架构树补 `auth.rs`；就绪判定改「200/30x/401 + 认证宽限」；下载说明补来源校验；`server.rs` 条目「供 npx」→「供 pnpm/npx」；`csp: null` 观察项记录（P3-10） |
| P3-8 | `showIpcError` 时禁用 `btn-retry`；`applyState` 正常路径恢复可用（含 title 提示） |
| P3-9 | `log()` 限长：超过 200 行移除最旧行（`while (logEl.childElementCount > 200) logEl.firstElementChild?.remove()`） |
| P3-10 | 不改，仅文档记录 |

---

## 三、执行与验证

1. 顺序：P1-1 → P2-2 → P2-1 → P3（一次性完成）；版本 0.1.29 三处同步 + README 更新记录
2. 新增/更新 `.tools/` 脚本：P1-1 冷启动序列 e2e、P2-1 分片 token e2e、P2-2 直连 401 引导 e2e
3. 回归：0.1.28 剪贴板（`cdp-copy-click-probe.mjs`）、认证正常链路（`cdp-copy-verify`/`cdp-auth-final`）、keep-alive 双按钮（用户实测）、多字节高负载输出（`verify-*` 脚本）
4. `scripts/publish.ps1` 出包至 `release/`；git 提交与 Release 按约定（Release 手工，如需代发可说明）
