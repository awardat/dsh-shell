//! ConPTY 终端会话：以真实 cmd.exe 交互式会话承载启动命令，
//! 前端 xterm.js 通过 IPC 读写。

use crate::job::JobObject;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::Path;

pub struct TerminalSession {
    pub master: Box<dyn MasterPty + Send>,
    pub writer: Box<dyn Write + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub job: Option<JobObject>,
}

/// 启动一个 cmd.exe ConPTY 会话。
/// 返回会话句柄与读取端（读取端应移入独立线程持续读）。
/// cmd.exe 绝对路径：优先 SystemRoot 环境变量（Windows on ARM / 非常规安装也正确）
fn cmd_exe_path() -> String {
    std::env::var("SystemRoot")
        .map(|root| format!("{}\\System32\\cmd.exe", root.trim_end_matches('\\')))
        .unwrap_or_else(|_| "C:\\Windows\\System32\\cmd.exe".into())
}

pub fn spawn(
    workdir: &Path,
    rows: u16,
    cols: u16,
) -> Result<(TerminalSession, Box<dyn Read + Send>), String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty 失败：{e}"))?;

    let mut cmd = CommandBuilder::new(cmd_exe_path());
    cmd.cwd(workdir);
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn cmd 失败：{e}"))?;
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("pty reader 失败：{e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("pty writer 失败：{e}"))?;

    // 进程树绑定作业：退出时整树清理。
    // 已知限制：portable-pty 在 spawn 后才允许 attach（无 pre-spawn 挂起能力），
    // spawn→attach 窗口内 cmd 自行派生的进程（如 AutoRun）不在作业内；
    // attach 失败会记录日志（不能静默——否则退出会遗留进程树）
    let job = JobObject::new();
    match &job {
        Some(j) => match child.process_id() {
            Some(pid) => {
                if !j.assign(pid) {
                    eprintln!("[dsh-ui] job attach failed for pid {pid}: 进程可能已退出或已在其他作业中");
                }
            }
            None => eprintln!("[dsh-ui] child has no process id; job not attached"),
        },
        None => eprintln!("[dsh-ui] JobObject creation failed; process tree may survive exit"),
    }

    Ok((
        TerminalSession {
            master: pair.master,
            writer,
            child,
            job,
        },
        reader,
    ))
}

impl TerminalSession {
    pub fn write(&mut self, data: &[u8]) -> Result<(), String> {
        self.writer.write_all(data).map_err(|e| e.to_string())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if let Err(e) = self
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
        {
            eprintln!("[dsh-ui] pty resize failed: {e}");
        }
    }

    /// 进程是否仍存活（非阻塞）。
    /// 注意：try_wait 是回收探测，会消费子进程退出状态——如需退出码须在本次调用缓存。
    pub fn alive(&mut self) -> bool {
        self.child.try_wait().map(|s| s.is_none()).unwrap_or(false)
    }

    pub fn kill(&mut self) {
        if let Err(e) = self.child.kill() {
            eprintln!("[dsh-ui] child kill failed: {e}（进程树可能残留）");
        }
    }
}
