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
        .map_err(|e| e.to_string())?;

    let mut cmd = CommandBuilder::new(cmd_exe_path());
    cmd.cwd(workdir);
    let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);

    let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    // 进程树绑定作业：退出时整树清理
    let job = JobObject::new();
    if let Some(j) = &job {
        if let Some(pid) = child.process_id() {
            let _ = j.assign(pid);
        }
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
    pub fn write(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let _ = self
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
    }

    /// 进程是否仍存活（非阻塞）
    pub fn alive(&mut self) -> bool {
        self.child.try_wait().map(|s| s.is_none()).unwrap_or(false)
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}
