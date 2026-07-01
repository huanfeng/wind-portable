//! 按可执行文件全路径查找 / 终止进程（对应 C# `ProcessHelper`）。
//!
//! 用 Toolhelp 快照枚举进程，OpenProcess + QueryFullProcessImageNameW 取全路径，按路径
//! 精确匹配——避免 `taskkill /IM` 误杀其他目录下的同名便携实例。Windows 专用模块。

use std::path::{Path, PathBuf};

use windows::core::PWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    PROCESS_ACCESS_RIGHTS, PROCESS_NAME_FORMAT,
};

const PROCESS_QUERY_LIMITED_INFORMATION: PROCESS_ACCESS_RIGHTS = PROCESS_ACCESS_RIGHTS(0x1000);
// PROCESS_TERMINATE(0x0001) | SYNCHRONIZE(0x0010_0000)。
const PROCESS_TERMINATE_SYNC: PROCESS_ACCESS_RIGHTS = PROCESS_ACCESS_RIGHTS(0x0001 | 0x0010_0000);
const PROCESS_NAME_WIN32: PROCESS_NAME_FORMAT = PROCESS_NAME_FORMAT(0);

/// 是否存在路径等于 `target` 的运行进程。
pub fn exists_by_path(target: &Path) -> bool {
    let target = target.to_string_lossy().to_lowercase();
    enumerate_pids()
        .into_iter()
        .any(|pid| process_path(pid).is_some_and(|p| p.to_string_lossy().to_lowercase() == target))
}

/// 终止所有路径等于 `target` 的进程；返回是否至少终止了一个。
pub fn terminate_by_path(target: &Path) -> bool {
    let target = target.to_string_lossy().to_lowercase();
    let mut stopped = false;
    for pid in enumerate_pids() {
        let matches =
            process_path(pid).is_some_and(|p| p.to_string_lossy().to_lowercase() == target);
        if matches && terminate_pid(pid) {
            stopped = true;
        }
    }
    stopped
}

/// 终止所有可执行文件名（不含扩展名）等于 `stem` 的进程；返回是否至少终止了一个。
///
/// 用于关闭占用管道的、来自其他目录的残留服务实例（按名兜底，对应 C# `ShutdownStaleService`）。
pub fn terminate_by_name(stem: &str) -> bool {
    let stem = stem.to_lowercase();
    let mut stopped = false;
    for pid in enumerate_pids() {
        let matches = process_path(pid).is_some_and(|p| {
            p.file_stem()
                .map(|s| s.to_string_lossy().to_lowercase() == stem)
                .unwrap_or(false)
        });
        if matches && terminate_pid(pid) {
            stopped = true;
        }
    }
    stopped
}

/// 枚举所有进程 pid。
fn enumerate_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return pids;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                pids.push(entry.th32ProcessID);
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    pids
}

/// 取 pid 的可执行文件全路径。
fn process_path(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let res = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        if res.is_ok() && size > 0 {
            Some(PathBuf::from(String::from_utf16_lossy(
                &buf[..size as usize],
            )))
        } else {
            None
        }
    }
}

/// 终止指定 pid；返回是否成功。
fn terminate_pid(pid: u32) -> bool {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_TERMINATE_SYNC, false, pid) else {
            return false;
        };
        let ok = TerminateProcess(handle, 0).is_ok();
        if ok {
            // 等待退出，至多 2s（句柄含 SYNCHRONIZE）。
            let _ = WaitForSingleObject(handle, 2000);
        }
        let _ = CloseHandle(handle);
        ok
    }
}
