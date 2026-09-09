# dsh-ui review.txt 整改方案

> 状态：✅ 已执行（0.1.27，2026 年随 CODE_REVIEW 整改完成）。
> 来源：[review.txt](review.txt)（62 条代码审查意见）。所有关键项均已对照源码核实，
> 部分与实测/文档证据有出入，见「不采纳」与「已处理」节。

---

## 一、采纳清单（按执行优先级）

### P0 — 崩溃 / 数据丢失 / 用户意图相反

| # | 位置 | 问题（核实属实） | 改法 |
|---|---|---|---|
| 1 | `server.rs decode_project_key` | workspace 为盘符根（`--C--`）时 `rest` 为空、`rest[0]` 越界 **panic**；发生在持 `AppInner` 锁期间 → 毒锁全进程失效 | `rest.is_empty()` 时直接返回 `[盘符根]` 候选（不枚举） |
| 2 | `server.rs handle_close` 保持运行分支 | 直接 spawn detached 新实例：当前 JobObject 内的服务仍占着端口 → detached 撞 EADDRINUSE 退出 → JobObject 退出杀旧树 → **用户选「保持运行」却无服务**；端口已被既往 detached 占用时再起一份是多余/有害 | spawn 前：杀当前会话树并等待端口释放（poll）；端口仍被占用（存活 detached/外部）→ 跳过 spawn 并日志说明 |
| 3 | `settings.rs load` | `from_str(...).unwrap_or_default()`：单个坏字段丢弃**整个**配置，且后续 save 把默认值写回 → 原配置永久丢失 | 区分「文件不存在」（返回默认）与「存在但解析失败」（备份为 `settings.json.bak-<ts>` + 日志 + 默认值继续） |

### P1 — 安全 / 认证正确性

| # | 位置 | 问题 | 改法 |
|---|---|---|---|
| 4 | `server.rs start_probe` | Ready 可早于 `auth_pending` 置位（reader 尚未解析到 token 行）：`!auth_pending` 与"旧版无需认证"不可区分 → iframe 无 cookie 401 且注入后无重载（竞态窗口真实存在） | HTTP 首见就绪后引入 grace（约 1.5s 数轮轮询），给 reader 时间置位 `auth_pending`/`auth_done` 再提交 Ready |
| 5 | `server.rs` 读取线程失败路径 | `eprintln!("... {web_url}")` 把 **launch token（凭据）写进日志** | 日志只打印 host:port，不打印 query（token） |
| 6 | `lib.rs on_navigation` | 放行任意 `data:`/`about:` 与任意端口的 127.0.0.1/localhost；加载内容可 `target=_top` 导航进特权窗口 | 收窄：仅放行 tauri:// + `http(s)://tauri.localhost` + **确切配置端口**的 loopback（`url.port() == settings.port`），`about:` 只留 `about:blank`；需回归直连/iframe/外链三路径 |
| 7 | `lib.rs` DevTools 门控 | `var("DSH_DEBUG_DEVTOOLS").is_ok()` 任意值（含 "0"）都开 9222 | 改为值 `"1"`/`"true"` 才开（release 调试通道保留，误继承环境变量不再裸奔） |
| 8 | `commands.rs open_browser` | 任意 scheme 直送 OS opener | 校验 `http`/`https` 后再打开 |
| 9 | `download.rs` 多下载权限 | 无条件 ALLOW `MULTIPLE_AUTOMATIC_DOWNLOADS`（任意来源可静默触发下载洪流） | 检查请求来源（`args.Uri()` 须为 loopback/本应用）再 ALLOW |
| 10 | `Cargo.toml` | 缺 `license`/`repository` 元数据 | `license-file = "../LICENSE"`、`repository = "https://github.com/awardat/dsh-shell"` |

### P2 — 可靠性 / 竞态 / 静默失败

| # | 位置 | 问题 | 改法 |
|---|---|---|---|
| 11 | `commands.rs save_settings` | 锁内阻塞 fs 写；save 失败仅 eprintln 仍返回 Ok（UI 报成功、磁盘旧值） | 序列化在锁内 → 释放锁后写盘；失败返回 `Err`（前端提示）；zoom.rs 同款处理 |
| 12 | `zoom.rs` | `step` 两次锁（check-then-act）；`clamp` 放行 NaN 并持久化 | step 单锁读-算-写；入口 `is_finite()` 拒绝 |
| 13 | `terminal.rs` | job attach 失败静默（`let _`）→ 可能残留进程树；kill/resize 失败不可见 | attach/创建失败记日志；kill/resize 错误 eprintln（保留接口签名） |
| 14 | `settings.rs save` | 共享固定 `settings.json.tmp`，并发写可 rename 撕裂文件 | tmp 名加进程 id+序号；模块级静态 `Mutex` 串行化保存 |
| 15 | `download.rs` | `SetResultFilePath/SetHandled` 失败时 `?` 提前返回 → 下载被取消且无任何事件；`add_*` 注册结果被吞；`sanitize_filename` 未挡 Windows 保留字/控制字符；`download_dir` 兜底盘符根（无权限写） | 失败仍 emit error 事件；注册结果检查+日志；sanitize 补 `< > " \| ? *`、控制字符、尾点空格、设备名（CON/PRN/AUX/NUL/COM1-9/LPT1-9）；兜底改 `temp_dir()` |
| 16 | `scripts/publish.ps1` | 只查 npm 退出码，可能发布**旧产物**；`HTTPS_PROXY` 未独立兜底 | 记录 `$buildStart`，只接受 `LastWriteTime >= $buildStart` 的安装包，无则报错；`HTTP_PROXY`/`HTTPS_PROXY` 各自检查；`Copy-Item` 覆盖行为在头部注释写明（同版本重发=覆盖为有意行为） |
| 17 | `server.rs` gen 校验 | ready 路径：gen 检查与副作用（清 restart_count/置 Ready/起 keepalive）分离多锁 | ready 提交前单锁内复查 `gen == gen && !probe_stop` 再继续 |
| 18 | `shell.ts` 终端快照 | 先 `getTerminalBuffer()` 后订阅 `onTerminalData` → 中间输出永久丢失 | 先订阅（缓冲到 pending）→ 写快照 → 顺序 flush pending |
| 19 | `shell.ts applyState` | 分支只更新子集，非相邻状态切换（failed→ready 等）残留 errorBox/spinner/按钮状态 | 分支前统一复位全部状态元素，再按 phase 显式设置 |
| 20 | `commands.rs get_settings` | `to_value().unwrap_or_default()` 序列化失败伪装成 Null | 返回 `Result` 传播错误 |
| 21 | `commands.rs terminal_input` | 会话不存在/写入失败静默吞字 | 返回 `Result<(), String>`（前端 console.error） |

### P3 — 整洁 / 文档 / 低危（随 P2 一起处理）

| # | 位置 | 改法 |
|---|---|---|
| 22 | `server.rs:746` 之外同类 | `settings.rs` load/save 入口做语义归一化（`port>0`、`ratio∈(0,1]`、`zoom` 有限、`use_system_proxy=false` 且 proxy_url 空时警告） |
| 23 | `index.html` | port/timeout 输入加 `step="1"`，保存路径整数化（`Math.trunc`）；`cfg-sysproxy` 去掉 markup 硬编码 `checked`（运行时以设置为准）；错误文本容器允许 `user-select: text` |
| 24 | `index.html` browserBtn | `failed` 分支也显示「在浏览器中打开」（当前默认 hidden 致首启失败只有重试可点） |
| 25 | `shell.ts` / `ipc.ts` | 保存设置只回传弹窗编辑字段（zoom/autoStart/ratio 不再由前端构造——后端已保留，前端同步清理）；`StateChangedPayload = AppState`；`DEFAULT_SETTINGS: Readonly<Settings>`；关键 IPC 调用补 `.catch` → `log()` |
| 26 | `job.rs` Sync 注释 | 更正论证：写明三条不变式（句柄不复制/CloseHandle 仅 Drop/内核并发安全） |
| 27 | `build.rs` | `rerun-if-changed` 补 `icons/icon.icns`、`icons/icon.png` |
| 28 | `.gitignore` | `.env.*`（保留 `!.env.example`）、OS/编辑器杂物（`.DS_Store`、`Thumbs.db`、`.idea/`、`.vscode/`、`*.tsbuildinfo`） |
| 29 | `ui/tsconfig.json` | `types` 补 `node`（或拆分 app/node 两个 tsconfig）；`isolatedModules: true` |
| 30 | `vite.config.ts` | `__dirname` → `fileURLToPath(new URL(".", import.meta.url))`（ESM 安全） |
| 31 | UI 启动 watchdog | 无状态事件 ~20s → 显示错误盒 + 「重新加载」按钮（模块加载失败可自救） |

---

## 二、已处理（review 未考虑现状，无需改）

- `shell.ts:352-356` 硬编码 `zoom:1.0/autoStart:true/terminalHeightRatio`：**0.1.24 起后端 `save_settings` 保留这三项磁盘原值**，前端占位值不会覆盖用户配置（保留代码仅为类型完整性）。本轮随 #25 顺带清理前端构造。
- `index.html:84` "checked 与运行时相反"：打开设置弹窗即用 `getSettings()` 覆盖勾选与禁用态，markup 初值仅首帧瞬间（#23 顺手清理）。
- `server.rs:837-840`（部分）：读取线程 FAIL_PATTERNS 路径 gen 检查后置位——随 #17 统一复查。

## 三、不采纳（附理由）

| # | 位置 | review 主张 | 结论 |
|---|---|---|---|
| A | `auth.rs Expires` 单位 | 声称应为 1601 FILETIME 纪元，现写 unix 秒 → cookie 已过期 | **不成立**：实测 `Network.getAllCookies` 返回 `expires≈1.79e9`（unix 2026 年）且 cookie 正常发送（iframe 跨站 RPC 200）；Microsoft Learn 明确 "since the UNIX epoch"。维持现值，注释已准确 |
| B | `Cargo.toml` windows 世代混用 | 0.59/0.61 与 tauri 栈 0.62/0.61 并存，建议升级统一 | 需连带升级 `webview2-com`（要求 0.61），风险大于收益；两代类型当前无交叉。在 DEVELOPMENT 已知问题补说明，**跟踪升级** |
| C | `terminal.rs` job attach 时序（spawn 后 attach，AutoRun 逃逸） | 建议 suspended + pre-attach | portable-pty API 不支持 pre-spawn attach（pty 内部 spawn）；缓解 = attach 失败可见（#13）。记录为已知限制 |
| D | `publish.ps1` 同版本覆盖 | 建议时间戳保留历史 | 同版本重发=重新发布同一版本，覆盖为**有意语义**；头部注释说明（#16） |

---

## 四、执行与验证

1. P0(3) → P1(7) → P2(11) → P3(10)，一次完成；版本 0.1.27 三处同步
2. 单测/脚本：decode_project_key 盘符根用例、sanitize 保留字用例（node 等价验证沿用 `.tools/` 模式）
3. e2e：
   - P0-2：keep-alive「保持运行」→ detached 存活且服务在（模拟 3999+dummy）
   - P1-4：0.1.2 认证启动仍一次就绪（真实 dsh 3999 复测，跑 3 次确认无 401 竞态）
   - P1-6：导航收窄回归（直连/改端口重启/iframe 加载/外链开浏览器）
   - P2-11：save 失败（只读目录）→ 前端收到错误提示
4. 发布：`scripts/publish.ps1` 出包至 release/，README 更新记录补 0.1.27
