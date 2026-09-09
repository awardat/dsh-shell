//! Windows Job Object：把 cmd 进程树绑进作业，句柄关闭（进程退出）时整树被杀。
//! 确保退出客户端后不会残留 npx/node 进程。

use std::ffi::c_void;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
    JobObjectExtendedLimitInformation, JOBOBJECT_BASIC_LIMIT_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

pub struct JobObject(HANDLE);

impl JobObject {
    pub fn new() -> Option<Self> {
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation = JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..std::mem::zeroed()
            };
            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                CloseHandle(handle);
                return None;
            }
            Some(JobObject(handle))
        }
    }

    /// 将指定 pid 的进程加入作业。返回是否成功。
    pub fn assign(&self, pid: u32) -> bool {
        unsafe {
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return false;
            }
            let ok = AssignProcessToJobObject(self.0, process);
            CloseHandle(process);
            ok != 0
        }
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

// `Sync` 与公开 `&self` 方法（assign）并存时，并发跨线程使用是允许的（如 Arc<JobObject>），
// 其安全性依赖三条不变式：
// 1) 原始 HANDLE 从不复制，CloseHandle 恰好执行一次（仅 Drop）；
// 2) Drop 由 Rust 所有权保证不与任何存活的 &self 借用竞争；
// 3) 对同一作业句柄的并发内核调用（OpenProcess / AssignProcessToJobObject）线程安全。
// 若未来添加非 Sync 字段或任何 &self 可变状态，必须重新评估本 impl。
unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}
