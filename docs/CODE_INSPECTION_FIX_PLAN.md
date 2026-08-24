# dsh-ui 代码检查整改方案（针对 CODE_INSPECTION_REPORT.md）

> 状态：待确认。确认后一次性完成修改，版本号第三段 +1 → **0.1.24**，重新打包。
> 对照报告：[CODE_INSPECTION_REPORT.md](CODE_INSPECTION_REPORT.md)（2026-08-17，检查版本 0.1.23）。
> 以下所有问题均已逐项核对源码，确认属实。

---

## P0 — 优先处理（崩溃 / 与用户意图相反的危险动作）

### P0-2 关闭询问框分支倒置（最紧急）

**核实**：`server.rs:855` `if ask_end_service(app) { /* 按端口杀 */ return; }`，
而 `ask_end_service`（`OkCancelCustom("保持运行","结束服务")` + `blocking_show`）点「保持运行」返回 `true`。
→ **用户点「保持运行」实际杀掉服务；点「结束服务」反而另起脱离进程保活**，完全颠倒。
0.1.16 引入，交付时未做对话框端到端验证，人工审查漏检。

**改法**（语义自洽，防再次倒置）：
1. 函数更名 `ask_end_service` → `ask_keep_alive`，文档注释明确「返回 true = 用户选择保持运行」；
2. 调用处重排为：
   ```rust
   if ask_keep_alive(app) {
       // 保持后台运行：spawn 脱离进程（现有逻辑原样搬入）
   } else {
       // 结束后台服务：按端口杀（现有逻辑原样搬入）
   }
   ```
3. 两个分支的注释同步纠正。

**验证**：原生对话框无法自动化点击 → 修复后**请用户实测两个按钮**（「保持运行」应后台保活、下次直连；「结束服务」应杀服务、下次重启服务）；代码侧以分支语义审查为准。

### P0-1 `recent` 滑动窗口字节切片 panic

**核实**：`server.rs:641-642` `recent[recent.len() - 4096..]` 按字节切片，含中文/ANSI 多字节输出累计 >8192 字节即可能 panic（上一轮修 `term_buffer` 时遗漏的同循环同款问题）。读取线程 panic → 失败特征扫描中止 + 终端实时输出中断。

**改法**：
1. 抽出纯函数 `trim_window(s: &str, keep: usize) -> String`（`floor_char_boundary` 截断保留后半段），`recent` 与（可选）`truncate_term_buffer` 共用；
2. 补单测（多字节字符反复填充触发截断，断言不 panic 且输出合法 UTF-8）。

**验证**：node 等价脚本（沿用 `.tools/verify-*.mjs` 模式，含中文负载用例）。

---

## P1 — 逻辑 / 可靠性

### P1-1 `dsh_available()` 与 pnpm 默认命令不匹配

**核实**：`server.rs:258-292` 只检测 PATH 中 `dsh` 与 npm `_npx` 缓存；0.1.22 起默认命令是
`pnpm dlx @deepseek-ai/dsh@next web --no-open`，pnpm 缓存不在这两处 → pnpm 用户每次启动误报
「正在下载 DeepSeek Harness」且超时放宽到 1800s。

**改法**：`dsh_available()` 增加第三路检测（静态路径，不跑子进程）：
- `%LOCALAPPDATA%\pnpm-cache\dlx\*\*\node_modules\@deepseek-ai\dsh\package.json`（pnpm dlx 缓存，两级通配）；
- `%LOCALAPPDATA%\pnpm\store\v11\links\@deepseek-ai\dsh\package.json`（pnpm store 链接，版本无关）。

**验证**：本机 pnpm 已缓存 rc.8（dlx + store 均有）→ 启动不再显示「正在下载」；node 等价脚本验证通配扫描逻辑。

### P1-2 改端口重启后 iframe 不重载

**核实**：`shell.ts:120-122` `loaded` 一次性置位，`frame.src` 只在首次 ready 赋值；改端口重启后 iframe 仍指向旧端口。

**改法**：
```ts
if (!loaded || frame.src !== p.url) {
  loaded = true;
  frame.src = p.url;
}
```

**验证**：e2e（CDP）：改端口 → 重启服务 → ready 后断言 `frame.src` 为新端口 URL。

---

## P2 — 质量 / 安全

### P2-1 设置保存覆写 `autoStart` / `terminalHeightRatio`

**核实**：`shell.ts:333/336` 保存时硬编码 `autoStart: true`、`terminalHeightRatio: 默认值`，
会静默覆盖用户在 settings.json 的手动设置。

**改法**（后端保留原值，前端无需感知）：`commands.rs::save_settings` 在转换后补：
```rust
s.auto_start = g.settings.auto_start;
s.terminal_height_ratio = g.settings.terminal_height_ratio;
```
（zoom 已按此模式保留）。

**验证**：e2e：settings.json 设 `auto_start: false` → 打开设置页保存 → 断言磁盘仍为 `false`。

### P2-2 下载文件名未清洗（路径穿越）

**核实**：`download.rs:178` `download_dir().join(&name)`，`name` 来自 Content-Disposition 原样值，
绝对路径/`..` 可逃逸下载目录（来源为本机 DSH 服务，实际风险低）。

**改法**：`resolve_filename` 输出前清洗：取 `Path::new(&name).file_name()` 末段、拒绝含 `..` /
盘符 / 分隔符的值，清洗后为空则回退时间戳命名；补单测（含 `/Windows/evil`、`../../x` 用例）。

### P2-3 `terminalHeightRatio` 形同虚设

**核实**：配置项在 Settings/前端/文档均存在，但面板高度 CSS 硬编码 55%（`styles.css:206`），无任何代码读取。

**改法**（接线，保留文档承诺）：`shell.ts` 启动时 `getSettings()` 后设置
`panel.style.height = `${ratio * 100}%``（CSS 55% 作兜底）。

**验证**：e2e：settings 改 ratio → 启动后面板高度按比例。

---

## P3 — 文档 / 一致性（随手改）

| # | 改法 |
|---|---|
| P3-1 | `capabilities/default.json`：`windows` 改 `["main"]`；description 去掉「浮层窗口」「缩放快捷键注入」等废弃描述（remote urls 保留，无害） |
| P3-2 | `方案.md` 顶部标注改为「历史设计稿（2026-08-14），正文部分描述已废弃架构，实现以 README / DEVELOPMENT 为准」 |
| P3-3 | `README.md` 安装包名 → `dsh_shell_0.1.24_x64-setup.exe`；更新记录加 0.1.24 条目（本轮全部修复） |
| P3-4 | `set_var` 多线程 footgun：**不改**（edition 2021 合法，低风险；在 DEVELOPMENT 已知问题补一句说明） |
| P3-5 | `settings.rs::save` 返回 `Result`：rename 失败清理 `settings.json.tmp` 并 eprintln（不静默） |
| P3-6 | `on_navigation` 放行 `data:`/`about:`：**保留**（WebView 内部导航兜底，低风险；注释说明） |
| P3-7 | README「设置」相关文案补充：代理（`use_system_proxy`/`proxy_url`）修改同样重启服务后生效 |

---

## 执行顺序与验证

1. P0-2 → P0-1 → P1-1 → P1-2（代码+单测/node 脚本）
2. P2 三项 + P3 文档项
3. 版本 0.1.24（三处同步）+ README 更新记录
4. 编译打包 `dsh_shell_0.1.24_x64-setup.exe`
5. e2e 验证：P1-1（无「正在下载」误报）、P1-2（iframe 重载）、P2-1（auto_start 保留）、P2-3（面板高度）；P0-1/P2-2 走 node 等价脚本；P0-2 请用户实测两个按钮
