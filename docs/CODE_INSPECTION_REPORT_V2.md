# dsh-ui 代码与文档检查报告（V2）

> 检查轮次：第 4 轮（接续 [CODE_QUALITY_REVIEW.md](CODE_QUALITY_REVIEW.md)、[CODE_INSPECTION_REPORT.md](CODE_INSPECTION_REPORT.md)@0.1.23、外部 [review.txt](review.txt) + [CODE_REVIEW_FIX_PLAN.md](CODE_REVIEW_FIX_PLAN.md)@0.1.27 之后）
> 检查基准版本：**0.1.28**（package.json / Cargo.toml / tauri.conf.json 三处同步一致）
> 检查范围：Rust 全部 10 文件（含新增 `auth.rs`，`server.rs` 重构至 1312 行，约 2,575 行）；前端 `ui/src` 全部（约 550 行 TS + 430 行 CSS）；配置/构建/发布（`Cargo.toml`、`tauri.conf.json`、`build.rs`、`capabilities/`、`vite.config.ts`、`tsconfig.json`、`scripts/publish.ps1`、`nsis/installer.nsi`）；文档（根 `README.md`、`docs/` 8 份）；测试资产（27 个 Rust 单测、`.tools/` 90+ 验证/CDP 脚本）；`LICENSE`
> 方法：逐文件人工审读；对两轮整改方案（31 项 + 10 项）逐项溯源核对落地；对 e2e 脚本的断言盲区做反推；与 DSH 源码比对关键约定（`deepseek-harness-master-0.1.2-rc.1`）；关键结论均有文件行号佐证
> 性质：**只读检查**，未修改任何代码

---

## 一、总体结论

1. **两轮整改落地质量高**：响应上轮报告的 0.1.24 整改（10 项）与响应 external review 的 0.1.27 整改（31 项）中，除 1 项有次生缺陷（见 P1-1）外全部正确落地，且大多配有单元测试或 `.tools/` 验证脚本。上一轮的 2 个 P0（关闭询问框分支倒置、UTF-8 字节切片 panic）修复实现经本次逐行复核**确认正确**。
2. **认证适配（0.1.26，新增 `auth.rs`）设计合理**：token 捕获 → Rust 侧 HTTP 交换 → WebView2 CookieManager 注入（SameSite=None + Secure + 30 天持久）链路与 DSH 0.1.2-rc.1 实际行为吻合（打印格式 `dsh web: <loopback>/?token=… (LAN: …)` 已对照上游 `packages/bundle/web-app/src/index.ts:280` 核实）。
3. **本轮新发现 1 个 P1**——改端口流程中 `iframe` 残留错误页：这是 0.1.24 对上轮 P1-2 修复所引入的**次生缺陷**，且当时的 e2e 验证环境恰好掩盖了该序列；另发现 **2 个 P2**（认证捕获的跨块健壮性、直连场景 401 无恢复路径）与 10 项 P3。

---

## 二、整改落地核对

### 2.1 上轮报告的整改（CODE_INSPECTION_FIX_PLAN → 0.1.24 / 0.1.25）

| 编号 | 状态 | 佐证 |
|---|---|---|
| P0-2 关闭询问框倒置 | ✅ 正确落地 | 函数更名 `ask_keep_alive`（`server.rs:1131-1146`），`handle_close:1054` 起分支语义正确：true=「保持运行」→ 停当前树 → 等端口释放（≤5s）→ 端口仍被存活进程占用则跳过重起 → 否则 spawn 脱离进程；false=「结束服务」按端口杀。含 review#2 的端口等待加固 |
| P0-1 `recent` 字节切片 panic | ✅ | 抽出纯函数 `trim_window`（`server.rs:54-61`，`floor_char_boundary`），`recent` 与 `truncate_term_buffer` 共用，含多字节单测 |
| P1-1 `dsh_available` 与 pnpm 不匹配 | ✅ | `server.rs:309-373` 增 pnpm dlx 缓存（`%LOCALAPPDATA%\pnpm-cache\dlx`）与 pnpm store 链接两路检测 |
| P1-2 改端口 iframe 不重载 | ⚠️ 落地但有次生缺陷 | `shell.ts:151-154` 按 URL 变化重载已实现，但真实冷启序列下不生效 → 本轮 **P1-1** |
| P2-1 保存覆写 autoStart/ratio | ✅ | `commands.rs:83-95` 后端保留三字段；前端 `shell.ts:387-403` 改为 `...base` 合并，不再硬编码 |
| P2-2 下载文件名未清洗 | ✅（并于 0.1.27 加固） | 见 2.2 #15 |
| P2-3 terminalHeightRatio 形同虚设 | ✅ | `shell.ts:263-268` 启动按设置设面板高度；e2e `cdp-e2e-0.1.24b.mjs` P2-3 断言 |
| P3-1~P3-7 | ✅ | capabilities 清理（`windows:["main"]`+描述）、`方案.md` 头部历史稿标注、README 0.1.24 条目、DEVELOPMENT set_var 注记、`write_settings` 返回 Result+失败清理、`data:`/`about:` 保留注释、README 代理「重启生效」注记（README:80-81） |

### 2.2 外部 review 的整改（CODE_REVIEW_FIX_PLAN → 0.1.27，31 项）

**P0（3/3）**：#1 盘符根 `decode_project_key` 越界（解码早退返回盘符根候选，`server.rs:127-`，含单测）✅；#2 keep-alive 与现服务抢端口（已并入 `handle_close`，见上）✅；#3 设置解析失败静默丢配置（`settings.rs:77-92` 备份为 `settings.json.bak-<ts>` + 日志）✅。

**P1（7/7）**：#4 认证就绪竞态（探针 2.5s 宽限 + `auth_pending` 等待 ≤8s + Ready 前单锁复查 gen，`server.rs:860-890`）✅；#5 token 不入日志（只打 host）✅；#6 导航收窄（`lib.rs:84-109`：仅 tauri:// + tauri.localhost + `about:blank` + **确切配置端口** 的 loopback）✅；#7 DevTools 门控（值 `1`/`true` 才开，`lib.rs:78-82`）✅；#8 `open_browser` 仅 http/https（`commands.rs:114-124`）✅；#9 下载多文件权限仅放行本机来源 ✅；#10 Cargo `license-file`/`repository`（`LICENSE` 为真实 MIT 文件）✅。

**P2（11/11）**：#11 设置锁外写盘 + 失败回滚上报（`commands.rs:75-112`）✅；#12 缩放 `is_finite` + step 单锁（遗留小问题 → 本轮 P3-1）；#13 终端 job attach/kill/resize 失败可见（`terminal.rs:61-72,110-114`）✅；#14 保存串行化 + 唯一 tmp（`settings.rs:7-9,103-` SAVE_LOCK/SAVE_SEQ）✅；#15 下载事件补发、sanitize 补保留字/控制字符/尾点空格、兜底 `temp_dir()`（8 单测）✅；#16 `publish.ps1` 以 `$buildStart` 校验只收本次构建产物（L31,50）✅；#17 ready 提交前单锁复查（`server.rs:880-886`）✅；#18 终端快照竞态（先订阅 pending → 快照 → 顺序 flush，`shell.ts:210-236`）✅；#19 applyState 统一复位幂等（`shell.ts:128-131`）✅；#20 get_settings 返回 Result ✅；#21 terminal_input 返回 Result ✅。

**P3（10/10）**：#22 `normalize()` 语义归一（`settings.rs:48-69`）；#23 `step="1"`/`clampInt`/`checked` 移除；#24 failed 态显示浏览器按钮（`shell.ts:162`）；#25 前端合并保存 + `StateChangedPayload=AppState` + `Readonly` + `.catch`；#26 job.rs 三不变式注释；#27 build.rs 三图标 rerun；#28 .gitignore 扩充；#29 tsconfig `types:["node"]`+`isolatedModules`；#30 vite `fileURLToPath`；#31 IPC watchdog（`shell.ts:109-119,188-205`；实现为复用既有「重新启动」按钮 +「关闭重开」指引文案，非方案文本中的独立「重新加载」按钮——设计内偏差，可接受）。

### 2.3 版本间增量（非整改驱动）

- **0.1.25** 终端下载/安装特征动态放宽超时（`server.rs:592-597` DOWNLOAD_PATTERNS + `798-813` 命中放宽，`verify-download-patterns.mjs`）✅
- **0.1.26** 认证适配（`auth.rs` 148 行 + reader 捕获 + 探针宽限；`cdp-auth-final.mjs`、真实 dsh 复测记录见方案）✅
- **0.1.28** iframe `allow="clipboard-read; clipboard-write"` 修复壳内复制代码块（`index.html:13-18`）✅

---

## 三、新发现问题

### P1-1 改端口「保存 → 重启」后主界面停留错误页（0.1.24 P1-2 修复的次生缺陷）

**涉及**：`ui/src/shell.ts:108,151-154` 与 `src-tauri/src/commands.rs:75-112`

机制链（真实使用序列）：

1. 用户在设置页改端口并保存：`save_settings` 落盘后**无条件 `emit_state`**（相位仍是 `ready`，URL 已是新端口）；
2. 前端 `applyState` ready 分支：`norm(frame.src) !== norm(p.url)` 成立 → `frame.src = 新端口`，**保存瞬间** iframe 即导航——此时新端口尚无服务（README 明示「重启后生效」），WebView2 渲染「无法访问」错误页；
3. 用户按提示点「⟳ 重启」：`Boot` → 服务在新端口就绪 → `Ready`；此时 `frame.src` 已等于新 URL → **条件不再成立，不再赋值**——iframe 停留在第 2 步的错误页；
4. `loaded`（`shell.ts:108`）自始至终不复位；此后任意次「⟳」均无法自救（URL 不变即不重载），**仅重启客户端可恢复**。

**e2e 为何没发现**：`.tools/cdp-e2e-0.1.24b/c.mjs` 验证该流程时，目标端口（3999）**预先**由 dummy server 监听（脚本头注释「dummy 在 3999」）——保存即导航时 dummy 已应答 200，页面正常；脚本仅断言 `frame.src` 字符串，恰好掩盖了「新端口冷启动」的真实序列。

**建议修法**（任选其一，或组合）：
- `applyState` 的非 ready 分支（failed/stopped/boot）复位 `loaded = false`，使**每次进入 ready** 都无条件赋值 `frame.src`（同端口重启场景也自然覆盖：SPA 本身不需要重载，重复赋值同 URL 无副作用或可先判相位切换）；
- 或 `save_settings` 不在相位未变时广播 state（避免"保存即提前跳转"），把导航留给 ready 进入时。

**验证建议**：新增 e2e——目标端口**初始不监听**，完整走「改端口 → 保存 → 等 Boot → ⟳ 重启 → Ready」，断言 iframe 的 OOPIF target 内 DOM 可交互（如读页面标题），而非仅 `frame.src`。

---

### P2-1 `dsh web:` token 行跨读块断裂时认证捕获丢失

**涉及**：`src-tauri/src/server.rs`（reader 线程，`extract_dsh_web_url(&text)` 按单次 8KB 读块匹配，L737-741 附近）

ConPTY 输出没有消息边界；`dsh web: http://…?token=…` 一行若恰好被切在两个读块（低概率、真实存在），两个 `text` 分片均不含完整的 needle+URL → `auth_pending` 永不置位 → 探针 2.5s 宽限后按「旧版无需认证」提交 Ready → iframe 无 cookie → 401（且无恢复路径，见 P2-2）。

**建议**：匹配改在 `recent.push_str(&text)` **之后**用累计滑窗（`recent` 保留 4096 字节，足够容纳输出行）进行，或维护跨块行缓冲；同时把 `g.auth_pending = true` 的置位（L746 附近，当前**无 gen 校验**；`auth_done` 的 done 回调已有校验）补上 gen 检查——否则被杀会话的陈旧 reader 可让新代探针空等满 8s 上限。

---

### P2-2 直连场景认证 cookie 缺失/失效时无恢复路径；`shell.ts:140` 注释指向不存在的机制

**触发场景**（直连 = 端口已被占用时的 connect-only 路径，无终端输出、不会捕获 token）：
- 用户曾在**外部终端**手工启动新版 dsh（≥0.1.2-rc.1，随机 process token），首次打开壳 → 直连 → WebView2 中从未注入过 cookie；
- 或注入 cookie 已过期（30 天 TTL）/ WebView2 配置被清理，而 keep-alive 后台服务仍在；
  （auth.rs 的设计前提是「签名 secret 持久于凭据，旧 cookie 跨 dsh 进程有效」——正常保活/重启链路无问题，上述场景才会缺 cookie。）

**现象**：iframe 加载 401，界面无任何引导；用户可行恢复仅剩「终端面板 ⏹ 停止（按端口杀）→ ⟳ 重启」，无提示指路。

**附带**：`shell.ts:140` 注释「`frame.src` 可能带认证重载的缓存破拆参数（`?_=…`）」——**该机制在产品代码中不存在**，仅是 `.tools/cdp-reload-test.mjs` 手工 CDP 实验的残留描述（该实验直接对 DOM 设 `f.src='…/?_=999'`）。注释误导后来者以为存在认证后重载路径。

**建议**：
- 直连分支探测到 401 时给出明确提示（如 phase=failed + 文案「服务要求认证，请用 ⟳ 重新启动服务以自动完成登录」），或检测 401 自动走 stop→restart 换取新 token；
- 至少修正 `shell.ts:140` 陈旧注释。

---

### P3 — 风格 / 健壮性 / 文档（10 项）

| # | 位置 | 问题与建议 |
|---|---|---|
| P3-1 | `zoom.rs:61-76` | `step` 先更内存/序列化、后 `set_zoom`；`set_zoom` 失败路径**不回滚内存态**（对比 `apply` 是先设窗口后更内存、失败天然一致）→ 失败瞬间内存=新、窗口=旧、磁盘=旧。后续任意缩放操作可自愈，影响小；建议失败分支同样回滚 |
| P3-2 | `server.rs` reader 线程 | 对 `term_buffer` 的写 append 无 gen 校验——被杀会话的尾部残余输出可混入新会话缓冲（首帧快照偶发脏行，表现轻微）。可在 append 前复查 gen（与同段 `term:data` emit 一致） |
| P3-3 | `server.rs:798-813` | `download_seen` 先锁查 gen、再锁写 `needs_download`/`ready_timeout` 两段锁 TOCTOU——陈旧 reader 可给新代误放宽超时（影响=多等一段时间）。建议单锁复合操作 |
| P3-4 | `server.rs boot()`（L603 附近） | `dsh_available()` 在 `port_listening` 判断**之前**执行；直连分支弃用其结果（`where` 子进程 + 两处目录扫描白跑）。建议移到端口空闲分支内 |
| P3-5 | `server.rs:1131-1146` | `blocking_show` 于主线程调用；tauri-plugin-dialog 文档明示 "should NOT be used when running on the main thread context"。Windows 原生模态对话框自带消息泵因此实测可用，但属依赖未文档化行为——注释现称「无死锁风险」未讲依据，建议补注前提（或迁 async 回调） |
| P3-6 | `README.md:28` | 「从 0 开始」安装包名仍写 `dsh_shell_0.1.24_x64-setup.exe`，当前 0.1.28——**同类滞后第 3 次复发**（0.1.22 滞后一次、上轮报告 P3-3 指出一次）。人工同步不可持续，建议文改为「下载最新版（见发布目录/发布页）」占位，或 `publish.ps1` 发布时自动回填版本号 |
| P3-7 | `docs/DEVELOPMENT.md` | 架构速览（L185-206）模块树**缺 `auth.rs`**；「就绪判定」（L219）仍写「以 HTTP 200 为准」——0.1.26 起实为 200/30x/401 + 认证宽限；「下载支持说明」（L127-141）未提 0.1.27 的来源校验；server.rs 条目（L194）「供 npx 下载」残留（现为 pnpm） |
| P3-8 | `shell.ts:113-119` | `showIpcError`（IPC 已亡）后 `btn-retry` 仍可点击，`void cmd.restartService()` 仅产生控制台 rejection、无用户反馈；建议 IPC 失败态禁用按钮 |
| P3-9 | `shell.ts:238-244` | `log()` 日志行无数量上限（长会话 DOM 无限增长）——延续项；建议超过 N 行丢弃最旧行 |
| P3-10 | `tauri.conf.json` `csp: null` | 本地资产 + 本机可信 iframe 场景可接受；作为观察项延续（导航已收窄、下载已校验来源，当前风险面可控） |

---

## 四、文档一致性核查

| 项 | 结论 |
|---|---|
| 版本三处同步 | ✅ 均 0.1.28 |
| README「更新记录」 | ✅ 0.1.24–0.1.28 齐全，逐条抽查与代码对应（0.1.24 五项修复、0.1.26 认证、0.1.27 六类整改、0.1.28 剪贴板） |
| 文档迁移（docs/） | ✅ README 尾部链接指向 `docs/DEVELOPMENT.md`、`docs/方案.md`；tauri.conf bundle 的 `../README.md` 仍指向根 README（存在） |
| `方案.md` 历史稿标注 | ✅ 头部已注明历史设计稿及以 README/DEVELOPMENT 为准 |
| `capabilities/default.json` | ✅ 单窗口 + 描述已更新（remote 标注「当前未使用」） |
| `LICENSE` / Cargo 元数据 | ✅ 真实 MIT；`license-file`/`repository` 指向正确 |
| DEVELOPMENT「已知问题」 | ✅ 覆盖 set_var、windows crate 两代并存、JobObject attach 窗口、OOPIF 页面级 autoscroll、additional_browser_args 覆盖语义——记录详实（架构速览滞后另见 P3-7） |
| 对照 DSH 源码 | ✅ `dsh web:` 打印格式（含 `(LAN: …)` 后缀）与 `extract_dsh_web_url` 匹配（上游 `packages/bundle/web-app/src/index.ts:280`）；`--no-open` 语义一致 |
| 测试资产 | 27 个 Rust 单测（server 15 / settings 4 / download 8）+ `.tools/` 90+ 验证/CDP 脚本，整改项基本都有对应验证脚本 |

---

## 五、总结与处置建议

| 级别 | 数量 | 关键项 |
|---|---|---|
| P1 | 1 | 改端口流 iframe 残留错误页（0.1.24 修复的次生缺陷，e2e 断言过弱被掩盖） |
| P2 | 2 | token 行跨读块断裂漏捕获 → 无认证 401；直连场景 cookie 缺失/失效无恢复 + 陈旧注释 |
| P3 | 10 | zoom step 失败不回滚、reader 跨代写入、README 包名三度滞后、DEVELOPMENT 架构节滞后等 |

**优先级建议**：

1. **P1-1**：修 `applyState` 的相位转换重载（复位 `loaded`），并按「三、P1-1」的冷启动序列补强 e2e（内容级断言而非 `frame.src` 字符串）——顺带把「保存即提前跳转死端口」的体验一并消除；
2. **P2-2**：直连 401 给出恢复引导（提示 ⟳ 或自动 stop+restart）；同步修正 `shell.ts:140` 注释；
3. **P2-1**：token 捕获改用累计滑窗匹配 + `auth_pending` 置位补 gen 校验（可与 P3-2/P3-3 的 reader 线程 gen 校验一并处理）；
4. P3 按节奏；P3-6（README 包名）建议机制化解决（脚本回填），避免第 4 次复发。

**修复后验证清单**（建议全部落为 `.tools/` 脚本，延续现有模式）：

- P1-1：新端口冷启动完整序列，断言 OOPIF DOM 可交互；
- P2-1：模拟 token 行跨块输出（dummy server 拼行分两次 flush），断言仍能捕获并注入；
- P2-2：构造「外部先起 3999 认证 dummy → 壳直连」场景，断言出现指引而非无声 401；
- P3-1：mock `set_zoom` 失败（如窗口关闭竞态），断言内存/磁盘回滚一致；
- 回归：keep-alive 双按钮、同端口崩溃重启、多字节高负载输出、下载保留字（已有单测）、发布脚本 dry-run。
