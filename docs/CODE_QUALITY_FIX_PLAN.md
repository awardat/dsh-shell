# dsh-ui 代码质量整改方案

> 依据：[CODE_QUALITY_REVIEW.md](CODE_QUALITY_REVIEW.md)（2026-08-15）
> 状态：**待人工确认**——确认后按本方案执行，执行完统一编译。

---

## 核对结论汇总

| # | 级别 | 核对结果 | 说明 |
|---|---|---|---|
| 1 | P0 | ✅ 属实（影响有限） | 两次独立 lock 存在 TOCTOU；单 Mutex 无死锁，影响=可能多跑一轮探测 |
| 2 | P0 | ✅ 属实 | `term_buffer[cut..]` 字节切片遇多字节 UTF-8 会 panic |
| 3 | P0 | ✅ 属实 | 设置页保存/读取的字段名 camelCase↔snake_case 不匹配 → **保存会重置配置、读取全为默认值**（此 bug 尚未被发现，因设置页未做端到端验证） |
| 4 | P1 | ✅ 属实（设计如此） | 端口占用即直连是**用户明确需求**；需保留直连，仅补提示 |
| 5 | P1 | ⚠️ 部分属实 | `restart()` 已设 `probe_stop=true` + `gen+=1`，旧 keepalive 醒来即退；窗口期仅多跑一次循环迭代，无实际危害 |
| 6 | P1 | ✅ 属实 | 改端口后不自动重启，新端口无服务 |
| 7 | P1 | ✅ 属实 | `start /b` spawn 失败被 `let _` 吞掉，用户无感知 |
| 8 | P2 | ✅ 属实 | `download.rs` unsafe 无说明 |
| 9 | P2 | ✅ 属实 | `job.rs` unsafe impl 论证不足 |
| 10 | P2 | ✅ 属实 | 无任何单元测试 |
| 11 | P2 | ✅ 属实 | 前端 catch 全空，IPC 失败静默 |
| 12 | P3 | ✅ 属实 | `AppInner.zoom` 硬编码 1.0（实际被 setup 的 apply 覆盖，语义歧义） |
| 13 | P3 | ✅ 属实 | cmd.exe 路径硬编码 |
| 14 | P3 | ✅ 属实（无害） | FAIL_PATTERNS 中文串不会命中正常输出，可移除 |
| 15 | P3 | ✅ 属实（无害） | vite 会重写路径，无实际问题 |
| 16 | P3 | ✅ 属实 | 仅 TS 类型标注问题 |

---

## 整改动作（按优先级）

### P0 — 必须先修

**A. 设置读写字段名统一（#3）**
- `src-tauri/src/settings.rs`：新增 `SettingsIpc`（`#[serde(rename_all = "camelCase")]`，字段同 `Settings`）+ 双向转换 `From<SettingsIpc> for Settings` / `From<Settings> for SettingsIpc`。
- `src-tauri/src/commands.rs`：`save_settings` 参数改为 `SettingsIpc`，转换后保存；`get_settings` 返回 camelCase JSON。
- 磁盘 `settings.json` 格式保持 snake_case 不变（已有文件兼容）。
- 验证：设置页打开回显、修改保存后文件内容正确、重启后生效。

**B. term_buffer 截断 UTF-8 安全（#2）**
- `src-tauri/src/server.rs`：截断点改用 `floor_char_boundary`（Rust 1.73+，GNU 工具链 1.97 可用），或提取纯函数 `truncate_term_buffer(&mut String)` 按字符边界截断。
- 该函数同时加单元测试（含多字节字符用例）。

**C. start_probe 锁合并（#1）**
- `server.rs`：`stop` 与 `gen` 的读取合并为一次 `lock`，消除 TOCTOU。

### P1 — 可靠性

**D. 端口占用直连补提示（#4，保留直连语义）**
- `server.rs` `boot()`：直连分支写入终端提示改为更明确："端口 X 已有服务监听，已直接连接；若页面异常，可能是非 DSH 服务占用"（保持"有监听即直连"需求不变）。

**E. 设置保存自动生效（#6）**
- `commands.rs` `save_settings`：`port` 变化时在 `probe_stop=true` 后直接调用 `server::restart()`；`startup_command`/`working_dir` 变化同样自动重启（前端 toast 提示"配置已保存，服务已重启"）。
- 保持 `keep_alive_on_exit`/`auto_restart` 等非启动项只保存不重启。

**F. keep-alive 线程窗口注释（#5）**
- 不改行为，在 `start_keepalive` 循环头补注释说明 stop+gen 双保险的退出语义。

**G. 后台脱离启动失败可见（#7）**
- `server.rs` `handle_close`：检查 `spawn()` 结果，失败时 `eprintln` + emit 事件（前端面板日志显示"后台服务启动失败"）；`cmdline` 构造保持引号转义。

### P2 — 质量

**H. unsafe 注释（#8、#9）**
- `download.rs`：为每处 unsafe 块补安全说明（COM 指针由 WebView2 生命周期保证、token 未注销的原因、闭包由 WebView2 持有）。
- `job.rs`：补 `unsafe impl Send/Sync` 论证（HANDLE 为内核句柄值；单所有权、仅 Drop 关闭一次；无内部可变状态）。

**I. 单元测试（#10）**
- 新增 `#[cfg(test)]`：`decode_project_key`（含 `--C-code-dsh-ui--` 还原、歧义候选、存在性过滤）、`resolve_filename`（引号/无引号/`filename*=UTF-8''`/sessionId 兜底）、`truncate_term_buffer`、`SettingsIpc` 转换。
- 运行 `cargo test` 验证。

**J. 前端错误反馈（#11）**
- `shell.ts`：所有空 `catch {}` 补 `console.error`（至少两项：设置读写、终端缓冲快照）。

### P3 — 风格

**K. 细节修正（#12–#16）**
- `server.rs`：`AppInner::new` 用 `settings.zoom` 初始化 `zoom`。
- `terminal.rs`：cmd.exe 路径改用 `SystemRoot` 环境变量 + `System32\cmd.exe`，兜底 `C:\Windows`。
- `server.rs`：移除 FAIL_PATTERNS 中的中文 `"启动失败"`。
- `ui/index.html`：`/src/styles.css` → `./src/styles.css`（vite 会正常处理）。
- `ui/src/ipc.ts`：`saveSettings` 返回类型 `invoke<void>`。

---

## 工作量与风险

- 全部改动约 200–300 行，均为小范围修改，无架构变化。
- 风险点：A（设置序列化）涉及磁盘格式兼容——已设计为磁盘格式不变，仅 IPC 层转换。
- 测试：`cargo test`（新增单测）+ 手动验证设置页读写、保存重启、终端输出高负载截断。
- 执行方式：确认后一次性完成全部代码修改，**统一编译一次**（版本号第三段 +1 → 0.1.15），重新打包。

---

## 待确认

1. 方案是否全部接受？如有不做项请标注编号。
2. E（设置保存自动重启）会自动重启服务——确认接受（可能打断正在进行的会话）。
3. 确认后版本号升至 0.1.15（遵循版本约定）。
