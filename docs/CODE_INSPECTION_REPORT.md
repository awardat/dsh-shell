# dsh-ui 代码与文档检查报告

> 检查时间：2026-08-17
> 检查版本：0.1.23
> 检查范围：全部源码（Rust `src-tauri/src/` 共 9 文件 ~1100 行；前端 `ui/src/` 共 3 文件 ~860 行；配置/构建 `Cargo.toml`、`tauri.conf.json`、`build.rs`、`nsis/installer.nsi`、`capabilities/default.json`、`vite.config.ts`、`tsconfig.json`；文档 `README.md`、`DEVELOPMENT.md`、`方案.md`）
> 方法：逐文件人工审阅 + 与上一轮报告（[CODE_QUALITY_REVIEW.md](CODE_QUALITY_REVIEW.md) / [CODE_QUALITY_FIX_PLAN.md](CODE_QUALITY_FIX_PLAN.md)）对照 + 关键结论查阅依赖 crate 源码佐证
> 性质：**只读检查**，未修改任何代码

---

## 一、整体评价

代码库规模小、模块划分清晰（生命周期 / 终端 / 缩放 / 下载 / 作业对象 / 设置 / 命令层各司其职），文档体系完整（用户文档 README、开发文档 DEVELOPMENT、原始设计 方案）。上一轮（2026-08-15）报告中的 **3 个 P0 与多数 P1/P2/P3 已在 0.1.15 整改中修复**，并补齐了单元测试。

但本轮在当前 0.1.23 代码上发现 **2 个新的 P0 级问题**（其一为上一轮同类 bug 的遗漏点，其二为关闭询问框分支倒置——会做出与用户点击相反的危险动作），以及若干 P1/P2/P3。建议优先处理两个 P0。

---

## 二、上一轮检查对照（已修复项确认）

| 上一轮编号 | 结论 | 当前证据 |
|---|---|---|
| #1 P0 start_probe 两次独立 lock 的 TOCTOU | ✅ 已修复 | `server.rs:676-679` 改为一次锁内同时读 `probe_stop` 与 `gen`；`start_keepalive:738-741` 同样处理并补注释 |
| #2 P0 `term_buffer` 字节切片遇多字节 UTF-8 panic | ✅ 已修复 | `server.rs:49-54` 抽出 `truncate_term_buffer`，用 `floor_char_boundary` 按字符边界截断；含单测 `truncate_buffer_respects_utf8_boundaries` |
| #3 P0 Settings camelCase↔snake_case 反序列化丢配置 | ✅ 已修复 | `settings.rs:62-114` 新增 `SettingsIpc`（`rename_all="camelCase"`）+ 双向 `From`；`commands.rs:67/74` 读写均走 `SettingsIpc`；磁盘仍 snake_case；含 4 个单测 |
| #4 P1 端口占用直连不验证 | ✅ 已处理（保留直连语义+补提示） | `server.rs:522` 直连分支写入明确提示「若页面显示异常，可能是非 DSH 服务占用该端口」 |
| #5 P1 keepalive 双线程窗口 | ✅ 已处理（注释+单锁双保险） | `server.rs:736-741` 注释说明 stop+gen 双保险；`restart()` 不再直接拉起 keepalive，仅 `start_probe` 就绪后才起新 keepalive，窗口极小 |
| #6 P1 改设置不触发重启 | ⚠️ 改为「提示重启后生效」（设计变更） | `commands.rs:71-89` 仅落盘+emit；`shell.ts:342-343` 提示「设置已保存，重启服务后生效」。README 0.1.15 记录此变更。可接受，但见 P3-7 |
| #7 P1 后台脱离启动失败被吞 | ✅ 已修复 | `server.rs:895-904` 检查 `spawn()` 结果，失败 `eprintln` + emit `keepalive:failed`；前端 `shell.ts:209-211` 显示日志 |
| #8/#9 P2 unsafe 缺论证 | ✅ 已修复 | `download.rs` 各 unsafe 块均有「安全：…」注释；`job.rs:63-67` 补 Send/Sync 论证 |
| #10 P2 无测试 | ✅ 已修复 | `server.rs`、`settings.rs`、`download.rs` 共 14 个 `#[test]`（decode_project_key、truncate、netstat 解析、reg 代理、normalize_proxy、disposition、percent_decode、SettingsIpc 往返） |
| #11 P2 前端空 catch | ✅ 已修复 | `shell.ts` 各 catch 均补 `console.error`（162/182/241/287/345） |
| #12 P3 zoom 硬编码 1.0 | ✅ 已修复 | `server.rs:59` `AppInner::new` 用 `settings.zoom` |
| #13 P3 cmd.exe 路径硬编码 | ✅ 已修复 | `terminal.rs:19-23` 优先 `SystemRoot` 环境变量，兜底 `C:\Windows` |
| #14 P3 FAIL_PATTERNS 中文串 | ✅ 已修复 | `server.rs:498-506` 已移除中文「启动失败」 |
| #15 P3 index.html 绝对路径 | ✅ 已修复 | `ui/index.html:7` 改为 `./src/styles.css` |
| #16 P3 ipc.ts 返回类型 | ✅ 已修复 | `ipc.ts:54` `invoke<void>` |

> 注：`cargo test` 因 Windows GNU 工具链已知问题无法本机运行（见 DEVELOPMENT.md「已知问题」），单测代码本身已就位，可在 MSVC/CI 跑。

---

## 三、新发现问题

### P0 — 崩溃 / 与用户意图相反的危险动作

#### P0-1. `recent` 滑动窗口按字节切片，多字节 UTF-8 可 panic
**位置**：`src-tauri/src/server.rs:640-643`（终端读取线程）

```rust
recent.push_str(&text);
if recent.len() > 8192 {
    recent = recent[recent.len() - 4096..].to_string();   // ← 字节切片
}
```

**问题**：`recent[recent.len() - 4096..]` 按字节索引。`text` 来自 `String::from_utf8_lossy`，是合法 UTF-8，但其中含中日韩等多字节字符时，`len() - 4096` 极易落在某个多字节字符中间 → `&str` 切片 **panic**（`byte index … is not a char boundary`）。

**与上一轮的关系**：这正是上一轮 #2 修复的同一类 bug，但上一轮只改了 `term_buffer`（`truncate_term_buffer` 用 `floor_char_boundary`），**遗漏了同一文件、同一读取循环里的 `recent` 滑窗**。

**触发条件**：成功启动路径上，终端持续输出且累计 >8192 字节后触发截断。`pnpm dlx` 下载 + dsh 启动日志在有中文/ANSI 内容时很容易超过 8192 字节，且此处位于 `failed_once` 之前的主路径——**正常成功启动即可触发**，并非边缘场景。

**影响**：读取线程 panic，失败特征扫描中止（`failed_once` 之后再无快速失败能力）；线程退出后 `terminal:exit` 事件发出，但终端实时输出中断，用户看不到后续日志。

**建议**：与 `truncate_term_buffer` 同样用 `floor_char_boundary`，或抽公共函数复用：
```rust
let keep = recent.len() - 4096;
let cut = recent.floor_char_boundary(keep);
recent = recent[cut..].to_string();
```

---

#### P0-2. 关闭询问框分支倒置：点「保持运行」反而结束后台服务
**位置**：`src-tauri/src/server.rs:842-905`（`handle_close`）与 `907-922`（`ask_end_service`）

`ask_end_service` 的文档注释明确写着返回值语义：

```rust
/// 返回 true = 保持后台运行（默认按钮），false = 同时结束后台服务。
fn ask_end_service(app: &AppHandle) -> bool {
    ...
    .buttons(MessageDialogButtons::OkCancelCustom(
        "保持运行".into(),   // OK 按钮（默认）
        "结束服务".into(),   // Cancel 按钮
    ))
    .blocking_show()        // 点 OK（保持运行）→ true
}
```

而其唯一调用方 `handle_close` 把 `true`（保持运行）走进了「结束后台服务」分支：

```rust
if ask_end_service(app) {            // true == 用户点了「保持运行」
    // 结束后台服务：按端口杀
    let port = ...;
    if port_listening(port) {
        match kill_by_port(port) { ... }   // ← 杀服务，且不 spawn 脱离进程
    }
    return;
}
// 保持后台运行：脱离作业，另起一个独立 cmd 会话   // ← false == 用户点了「结束服务」却走到这里
... spawn 脱离进程 keep-alive
```

**依赖 crate 源码佐证**（已查阅 `tauri-plugin-dialog 2.7.2`）：
- `lib.rs:353`：`blocking_show` ——「Returns `true` if the user pressed the OK/Yes button」；
- `desktop.rs:232-235`：`OkCancelCustom(ok, _cancel)` 在 rfd 返回 `Ok` 时映射到第一个按钮（`ok` = 「保持运行」）。

故 `ask_end_service()` 在用户点「保持运行」时返回 `true`，与函数自身文档一致；问题出在 `handle_close` **把 `true` 当作「结束服务」处理**——调用方与被调用方的契约相反。

**实际用户可见后果（完全颠倒）**：
| 用户点击 | 期望 | 实际（当前代码） |
|---|---|---|
| 「保持运行」（默认按钮） | 后台保留服务，下次直连 | **杀掉端口上的服务**，且不 spawn 脱离进程 → 服务彻底结束 |
| 「结束服务」 | 结束后台服务 | **另起一个脱离作业的后台 cmd 进程**继续运行服务 → 服务反而在后台留着 |

这直接违背用户在一个「破坏性确认」对话框上的明确选择，且作用在 keep-alive 这一主推特性上。

**建议**：取反条件（语义最小改动）：
```rust
if !ask_end_service(app) {
    // 用户选「结束服务」：按端口杀
    ...
    return;
}
// 用户选「保持运行」：spawn 脱离进程
...
```
或把函数更名为 `ask_keep_alive` 使返回值与分支语义自洽。修复后务必端到端验证两个按钮各走对分支。

---

### P1 — 逻辑缺陷 / 可靠性

#### P1-1. `dsh_available()` 仍按 npx 缓存检测，与 0.1.22 起的 pnpm 默认命令不匹配
**位置**：`src-tauri/src/server.rs:260-295`（esp. 268-292 的 `_npx` 缓存扫描）

`dsh_available()` 通过两路判断系统是否已有 dsh：
1. `where dsh`（PATH 中有无 dsh 可执行）；
2. npm 的 `_npx/<hash>/node_modules/@deepseek-ai/dsh` 缓存是否存在。

但自 **0.1.22 起默认启动命令已从 `npx` 切换为 `pnpm dlx @deepseek-ai/dsh@next web --no-open`**（`settings.rs:26`、`ipc.ts:29`、README 0.1.22）。pnpm dlx 的缓存不在 npm 的 `_npx` 目录；而 dsh 也不会被装到 PATH。因此对使用 pnpm 默认、且从未用过 npx 的用户，`dsh_available()` **几乎每次启动都返回 false** → `needs_download=true`：

- 启动遮罩显示「正在下载 DeepSeek Harness（首次运行，可能需要数分钟到数十分钟）…」（`server.rs:547`），即便 pnpm 早已缓存、实际数秒即就绪；
- 就绪超时被放宽到 1800s（无害但语义错误）。

**影响**：每次启动都给出与事实不符的「正在下载」提示，误导用户以为在下载；首次运行检测逻辑未随 npx→pnpm 迁移同步更新。

**建议**：检测逻辑改为与实际启动命令对齐——例如解析 `startup_command` 选用包管理器后查对应缓存（pnpm store / `pnpm store path`、或 `where pnpm`+缓存探测），或退一步：不在启动命令层面做「是否需要下载」的精确判定，改为统一放宽超时 + 文案不写死「下载」，由实际就绪速度自然过渡。

---

#### P1-2. 改端口后重启，iframe 不会重新加载
**位置**：`ui/src/shell.ts:120-123`

```rust
if (p.phase === "ready") {
  ...
  if (!loaded) {
    loaded = true;
    frame.src = p.url;   // 仅首次 ready 时设置一次
  }
}
```

`loaded` 一旦置位就不再赋值 `frame.src`。流程：设置页改端口 → 保存（提示重启生效）→ 点「重新启动」→ `boot`→`ready`，state 携带新 `url`，但 `applyState` 因 `loaded===true` 跳过 `frame.src` 赋值 → iframe 仍指向旧端口（无服务/无关页面），用户看到空白或旧页。

**建议**：当 `p.url !== url` 时重置 `frame.src`（即使已 loaded），或在重启路径上重置 `loaded`。

---

### P2 — 代码质量 / 安全

#### P2-1. 设置保存把 `autoStart` 固化为 true、`terminalHeightRatio` 固化为默认值
**位置**：`ui/src/shell.ts:327-339`

```ts
const s: Settings = {
  ...
  autoStart: true,                                    // ← 恒为 true
  ...
  terminalHeightRatio: DEFAULT_SETTINGS.terminalHeightRatio,  // ← 恒为默认
  ...
};
```

设置弹窗（`index.html:54-106`）没有「自动启动」开关，保存时前端硬编码 `autoStart: true` 上送。若用户曾手动在 `settings.json` 里设 `auto_start: false`，打开一次设置页点保存即被静默改回 `true`。`terminalHeightRatio` 同理被覆写为默认（虽该项实际未生效，见 P2-3）。

**建议**：保存前先 `getSettings` 回显真实值（含 autoStart），或后端 `save_settings` 对前端未提供的字段保留原值（只更新 UI 暴露的字段）。

---

#### P2-2. `resolve_filename` 未做路径清洗，Content-Disposition 文件名可路径穿越
**位置**：`src-tauri/src/download.rs:94-123`（解析）+ `178`（`download_dir().join(&name)`）

`resolve_filename` 把 Content-Disposition 的 `filename` 原样作为文件名，随后 `download_dir().join(&name)`：
- 若 `filename="/Windows/evil"` 等绝对路径，`PathBuf::join` 会**替换**基目录；
- 若 `filename="../../../../Users/x/evil"`，可向上穿越。

下载来源是本机 DSH 服务（127.0.0.1:3080），用户自有/可信，实际风险低；但解析逻辑未做基本防护（剥离路径分隔符、拒绝绝对路径、去除 `..`）。

**建议**：对 `name` 做清洗——`Path::new(&name).file_name()` 取末段，丢弃含 `..`/盘符/分隔符的值，再兜底时间戳。

---

#### P2-3. `terminalHeightRatio` 配置项形同虚设
**位置**：`ui/src/styles.css:206`（`.panel { height: 55% }` 硬编码）vs `settings.rs:34`/`ipc.ts:37`/方案/README

配置项在 `Settings`/`SettingsIpc`/前端类型与文档中均存在，但面板高度在 CSS 中写死 55%，**没有任何代码读取该值**。属「存而不用」的配置，文档却把它列为可配置项，具误导性。

**建议**：要么接线（启动时按比例设面板高度），要么移除该字段与文档条目。

---

### P3 — 风格 / 文档 / 一致性

| # | 位置 | 问题 |
|---|---|---|
| P3-1 | `src-tauri/capabilities/default.json:5` | `windows: ["main", "overlay"]` 含不存在的 `overlay` 窗口（单窗口架构已废弃浮层窗口）；`description` 仍提及已废弃的「缩放快捷键注入」。stale 但无害 |
| P3-2 | `方案.md:3,99-112,146,154,172,224` | 顶部标注「已实施完成（2026-08-14）」，但正文描述的是**已废弃架构**：`overlay.rs` 浮层窗口（源码不存在）、`npx --yes @deepseek-ai/dsh web`（已改 pnpm）、主界面「不用 iframe 顶层加载」（实际为 iframe）、Ctrl+滚轮 `webview.eval` 注入缩放（已废弃，见 DEVELOPMENT「缩放注入说明」）。作为历史设计稿可保留，但「已实施完成」标注具误导性，建议改为「历史设计稿，实现以 README/DEVELOPMENT 为准」 |
| P3-3 | `README.md:29` | 「从 0 开始」步骤 2 让用户获取 `dsh_shell_0.1.22_x64-setup.exe`，当前版本已 0.1.23（仓库内亦有 0.1.23 安装包），版本号滞后一行 |
| P3-4 | `src-tauri/src/server.rs:379-402` | `apply_proxy_env` 在多线程下调用 `std::env::set_var/remove_var`（进程全局环境、非同步；Rust 2024 edition 起为 `unsafe`）。当前 edition 2021 可编译，属已知 footgun，低风险 |
| P3-5 | `src-tauri/src/settings.rs:55-57` | `save` 中 `let _ = fs::rename(tmp, path)` 忽略 rename 失败：若目标被占用导致 rename 失败，配置**静默未保存**且残留 `settings.json.tmp`。建议失败时清理 tmp 并返回 `Result` |
| P3-6 | `src-tauri/src/lib.rs:84-88` | `on_navigation` 放行 `data:`、`about:` scheme 的顶层导航。低风险，可保留，但如无需要可收紧为仅 `tauri`/`http(127.0.0.1|localhost)` |
| P3-7 | 文档 | 设置「重启后生效」说明未明确覆盖**代理**修改：改 `use_system_proxy`/`proxy_url` 同样需重启服务才注入到子进程环境，README/设置页文案未点出 |

---

## 四、文档一致性核查

| 项 | 结论 |
|---|---|
| 版本号三处同步（`package.json` / `Cargo.toml` / `tauri.conf.json`） | ✅ 均 0.1.23 |
| README「更新记录」与版本对应 | ✅ 当前 0.1.23 有条目 |
| README / DEVELOPMENT 架构描述与实现一致 | ✅ 单窗口+iframe、pnpm、ConPTY、Job Object、keep-alive、下载支持等均与源码相符 |
| `方案.md` 与实现一致 | ❌ 见 P3-2，显著过时且标注误导 |
| capabilities 与实现一致 | ⚠️ 见 P3-1，含废弃窗口与描述 |
| 安装包命名规范 `dsh_shell_<版本>_x64-setup.exe` | ✅ 仓库内 0.1.14→0.1.23 多版本命名一致 |
| 文档互链（README↔DEVELOPMENT↔方案） | ✅ 完整 |

---

## 五、总结与优先级建议

| 级别 | 数量 | 关键项 |
|---|---|---|
| P0（崩溃/危险动作） | 2 | `recent` 字节切片 panic；关闭询问框分支倒置（点「保持运行」反杀服务） |
| P1（逻辑/可靠性） | 2 | `dsh_available` 仍查 npx 缓存与 pnpm 默认不符；改端口后 iframe 不重载 |
| P2（质量/安全） | 3 | 设置保存覆写 `autoStart`；下载文件名未清洗可路径穿越；`terminalHeightRatio` 形同虚设 |
| P3（风格/文档） | 7 | 方案.md 过时误导、capabilities 含废弃窗口、README 安装包版本滞后等 |

**建议处理顺序**：

1. **P0-2**（关闭询问框倒置）——影响主推特性、违背用户明示选择，最紧急；改一行取反条件即可，但必须端到端验证两按钮分支。
2. **P0-1**（`recent` 字节切片 panic）——与上一轮已修的 `term_buffer` 同类，复用 `floor_char_boundary` 即可，并补单测。
3. **P1-1**（pnpm 缓存检测）——消除每次启动的误导性「正在下载」提示。
4. **P1-2**（iframe 重载）——修复改端口重启后的空白页。
5. 其余 P2/P3 按节奏处理；文档类（P3-2/P3-3）随手更新即可。

**修复后验证建议**：
- `cargo test`（MSVC/CI 环境）跑全部 14 个单测，并为 P0-1 补 `recent` 截断单测；
- 手动端到端：keep-alive 两按钮分支、改端口重启、含中文高负载终端输出、设置页保存后 `auto_start` 是否被覆写、下载文件名含 `..`/绝对路径的处置。

---

## 附：关键结论的依赖源码佐证

P0-2 的对话框返回值语义，已查阅本机 cargo 注册表中的 `tauri-plugin-dialog 2.7.2` 源码确认：
- `…/tauri-plugin-dialog-2.7.2/src/lib.rs:353`：`blocking_show` ——「Returns `true` if the user pressed the OK/Yes button」；
- `…/tauri-plugin-dialog-2.7.2/src/desktop.rs:232-235`：`OkCancelCustom(ok, _cancel)` 在 rfd 返回 `Ok` 时映射到第一个按钮文本（即 `ok`）。

由此 `OkCancelCustom("保持运行", "结束服务")` 点「保持运行」→ `blocking_show()` 返回 `true`，与 `ask_end_service` 文档注释「返回 true = 保持后台运行」一致；而 `handle_close` 将 `true` 走入「结束后台服务」分支，构成倒置。
