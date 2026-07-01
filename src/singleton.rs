//! 单实例互斥与"唤起已有窗口"（对应 C# 的 Mutex + ShowEvent）。Windows 专用。
//!
//! 首实例持有命名 Mutex（进程生命周期内保活）。后启动的实例发现 Mutex 已存在 → 直接用
//! Win32 找到首实例窗口并前置（后启动者本就持有用户输入前台权限，`SetForegroundWindow`
//! 可靠生效），随即退出。无需事件 + 等待线程，规避 windui 单线程模型下的跨线程唤醒难题。

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
};

use crate::variant::Variant;

/// 单实例守卫：句柄在进程生命周期内保活，Drop 时释放。
pub struct InstanceGuard {
    handle: HANDLE,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// 尝试取得单实例所有权。已有实例运行时返回 None。
pub fn acquire(variant: &Variant) -> Option<InstanceGuard> {
    let name = wide(&variant.mutex_name);
    unsafe {
        let handle = CreateMutexW(None, true, PCWSTR(name.as_ptr())).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return None;
        }
        Some(InstanceGuard { handle })
    }
}

/// 把已有实例的窗口前置（按标题查找）。
pub fn activate_existing(title: &str) {
    let t = wide(title);
    unsafe {
        if let Ok(hwnd) = FindWindowW(PCWSTR::null(), PCWSTR(t.as_ptr())) {
            if !hwnd.is_invalid() {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
