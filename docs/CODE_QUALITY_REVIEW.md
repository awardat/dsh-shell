# dsh-ui 代码质量检查报告

> 检查时间：2026-08-15
> 检查范围：全部源码（Rust ~1100 行，TS ~400 行，CSS ~360 行）

## 一、整体评价

代码库规模小，结构清晰，模块划分合理。功能完整度高，没有明显的架构缺陷。以下按严重程度分级列出发现的问题。

---

## 二、P0 — 潜在崩溃 / 数据损坏风险

### 1. Mutex 死锁风险 — `server.rs` 多处重复获取锁

`start_probe` 线程中对 `inner` 的 lock 调用模式有隐患：

```rust
// 第406-407行：连续两次 lock，中间没有任何保护
let stop = { inner.lock().unwrap().probe_stop.clone() };
if stop.load(Ordering::Relaxed) || inner.lock().unwrap().gen != gen {
```

两次 `lock()` 是独立的，在极端调度下第二个 `lock()` 可能与读线程的 `lock()` 产生竞争。虽然 `Mutex` 不会死锁（单锁），但 `probe_stop.clone()` 后立即释放锁再重新获取，此时状态可能已被其他线程修改（TOCTOU）。

**影响**：可能导致探针线程在状态已变后仍继续执行，产生无效的 phase 广播。

---

### 2. `term_buffer` 环形缓冲截断不安全 — `server.rs:364-365`

```rust
if g.term_buffer.len() > TERM_BUFFER_MAX {
    let cut = g.term_buffer.len() - TERM_BUFFER_MAX / 2;
    g.term_buffer = g.term_buffer[cut..].to_string();
}
```

`String` 切片按字节索引，而 `from_utf8_lossy` 产出的字符串可能含多字节 UTF-8 字符。如果 `cut` 落在多字节字符中间，`&g.term_buffer[cut..]` 会 **panic**。

**影响**：高负载终端输出时可能 crash。

---

### 3. `download.rs` — `resolve_filename` 解析 Content-Disposition 不完整

```rust
// 第46-51行
let name = rest
    .trim_start_matches('"')
    .split('"')
    .next()
    .unwrap_or("")
```

只处理了双引号包裹的 `filename="xxx"`，未处理无引号的 `filename=xxx`（RFC 6266 允许）。此外未处理 `filename*=UTF-8''...` 编码形式。这不是崩溃风险，但会导致文件名解析失败时回退到时间戳命名。

---

## 三、P1 — 逻辑缺陷 / 可靠性问题

### 4. `boot()` 端口占用时跳过启动但不验证服务可用 — `server.rs:265-276`

```rust
if port_listening(port) {
    // TCP 有监听即直连，不验证 HTTP 200
    set_phase(app, inner, Phase::Ready, None);
```

端口被非 DSH 程序占用（如另一个服务碰巧用了 3080）时，会直接进入 Ready 状态，iframe 加载后显示一个完全无关的页面，用户体验差且难以排查。

**建议**：端口占用时应追加一次 `http_ok()` 验证，或在 UI 上提示"端口被占用，可能不是 DSH 服务"。

---

### 5. `keepalive` 线程无退出时序保证 — `server.rs:450-478`

`start_keepalive` 中 `fails >= 3` 触发 `fail_or_restart` 后直接 `return`，但此时 `fail_or_restart` 可能调用 `restart()` → `start_session()` → `start_probe()` → `start_keepalive()`，形成递归 spawn。虽然有 `gen` 机制让旧线程退出，但如果旧线程在 `sleep(2s)` 期间，新线程已经跑完一轮 probe 并进入 keepalive，旧线程醒来后才检查 gen，这段窗口期内会有两个 keepalive 线程并行。

**影响**：不会崩溃，但可能导致重复的 fail_or_restart 调用（被 `restart_count` 限制住）。

---

### 6. `save_settings` 不触发服务重启 — `commands.rs:66-85`

修改 `startup_command` 或 `working_dir` 后，需要用户手动点"重新启动"才能生效。但修改 `port` 只是停止探针（`probe_stop = true`），没有自动重连或重启。用户改完端口后会看到服务仍在旧端口运行，新端口无响应。

**影响**：功能可用性降低，用户可能困惑。

---

### 7. `handle_close` 中 `keep_alive_on_exit` 的实现用 `cmd /c start /b` — `server.rs:561-563`

```rust
let _ = std::process::Command::new("cmd")
    .args(["/c", "start", "\"\"", "/b", "cmd", "/c", &cmdline])
    .spawn();
```

`start /b` 在某些 Windows 版本/环境下行为不一致，且如果 `cmdline` 中包含特殊字符（如 `&`、`|`），命令解析可能出错。更关键的是，`let _ =` 忽略了 spawn 失败，后台服务可能根本没有启动成功但用户无感知。

---

## 四、P2 — 代码质量 / 可维护性

### 8. `Settings` 字段命名风格不一致 — `settings.rs` vs `ipc.ts`

Rust 端用 snake_case（`startup_command`），TypeScript 端用 camelCase（`startupCommand`）。serde 默认不做转换，但 `ipc.ts:49` 的 `saveSettings` 传入的 JS 对象字段名是 camelCase，而 Rust 的 `Settings` 结构体是 snake_case。

**实际运行无问题**：因为 `#[serde(default)]` + serde 默认 snake_case，但 TS 端传 camelCase 的 `Settings` 对象会被 serde 反序列化为全部使用默认值（所有字段都"缺失"），然后 `save` 会把错误的默认值写入配置文件。

**验证**：`save_settings` 接收 `crate::settings::Settings`，如果前端传的是 `startupCommand`（camelCase），serde 会因为找不到 `startup_command` 字段而使用 Default，配置会被重置。

**这是 P0 级别 bug** — 除非 serde 或 Tauri IPC 层做了 camelCase 映射。需要验证 Tauri 的 serde 序列化行为。

---

### 9. `download.rs` 大量 `unsafe` 代码缺少注释

整个 `download.rs` 有约 30 行 unsafe 代码，操作 WebView2 COM 接口。虽然 COM 调用本身无法避免 unsafe，但缺少对安全性的简要说明（如：为什么这些指针是有效的、生命周期如何保证）。

---

### 10. `job.rs` — `unsafe impl Send/Sync` 缺少论证

```rust
unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}
```

`HANDLE` 是 `isize`，确实可以 Send/Sync，但注释只有一行。如果未来 `JobObject` 增加字段，这个 unsafe impl 可能不再成立。

---

### 11. 无测试代码

整个项目没有测试文件（Rust 无 `#[cfg(test)]`，无 `tests/` 目录；前端无测试）。对于一个涉及进程管理、PTY、WebView2 COM 交互的应用，关键路径（如 `decode_project_key`、`resolve_filename`、状态机转换）应有单元测试。

---

### 12. 前端 `shell.ts` 无错误边界

所有 IPC 调用都用 `try/catch` 包裹但 catch 块为空（`catch { }`）。IPC 失败时静默忽略，用户无任何反馈。建议至少在控制台输出错误。

---

## 五、P3 — 风格 / 建议

| # | 位置 | 问题 |
|---|---|---|
| 13 | `server.rs:42-58` | `AppInner::new` 中 `zoom` 硬编码 `1.0`，但 settings 里有 `zoom` 字段，应读取 `settings.zoom` |
| 14 | `terminal.rs:33` | cmd.exe 路径硬编码 `C:\Windows\System32\cmd.exe`，在 Windows on ARM 或非常规安装下可能失败 |
| 15 | `server.rs:250-259` | `FAIL_PATTERNS` 包含中文 `"启动失败"`，但这个字符串来自用户配置的启动命令输出，不一定出现 |
| 16 | `ui/index.html:7` | `<link rel="stylesheet" href="/src/styles.css" />` 使用绝对路径，Vite dev server 能处理，但语义上应为 `./src/styles.css` |
| 17 | `ipc.ts:49` | `saveSettings` 返回类型标注为 `Settings`，但 Rust 端返回 `Result<(), String>`，类型不匹配 |

---

## 六、总结

| 级别 | 数量 | 关键项 |
|---|---|---|
| P0（崩溃/数据损坏） | 3 | Mutex TOCTOU、UTF-8 切片 panic、Settings camelCase 反序列化可能丢配置 |
| P1（逻辑缺陷） | 4 | 端口占用误判、keepalive 双线程窗口、settings 修改不触发重启、后台启动静默失败 |
| P2（代码质量） | 5 | 命名风格不一致、unsafe 缺论证、无测试、无错误反馈 |
| P3（风格建议） | 5 | 硬编码路径、默认值不读 settings 等 |

**优先处理建议**：

1. 验证并修复 Settings 序列化/反序列化的 camelCase/snake_case 问题（可能是 P0）
2. 修复 `term_buffer` 截断的 UTF-8 安全性
3. 为 `decode_project_key` 和 `resolve_filename` 添加单元测试
