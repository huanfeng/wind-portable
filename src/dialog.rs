//! 通用对话框：文件选择 / 目录选择 / 消息框。
//!
//! - 文件/目录选择走 windui 的 [`PickDialog`]（底层 `rfd` → 现代 `IFileDialog`），
//!   框架自动注入当前窗口为父窗口，主窗口在对话框期间被禁用、不会点击穿透——
//!   相比旧的 `GetOpenFileNameW`/`SHBrowseForFolderW` 更现代且规避了 owner/焦点问题。
//! - 消息框仍用 Win32 `MessageBoxW`（库未提供等价接口），需显式传 owner 保证模态。
//!
//! **均在 UI 线程调用**（模态，自带消息循环）。Windows 专用。

use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use windui::prelude::PickDialog;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MessageBoxW, IDOK, MB_ICONERROR, MB_ICONQUESTION, MB_OKCANCEL, MESSAGEBOX_RESULT,
    MESSAGEBOX_STYLE,
};

/// 按标题找启动器窗口（用作消息框 owner，保证模态）。
pub fn find_main_window(title: &str) -> Option<HWND> {
    let t = wide(title);
    unsafe { FindWindowW(PCWSTR::null(), PCWSTR(t.as_ptr())).ok() }.filter(|h| !h.is_invalid())
}

/// 选择 ZIP 文件。取消返回 None。父窗口由 windui 自动注入。
pub fn pick_zip(title: &str) -> Option<PathBuf> {
    PickDialog::new()
        .title(title)
        .filter("ZIP 压缩包", &["zip"])
        .filter("所有文件", &["*"])
        .pick_file()
}

/// 选择目录。取消返回 None。父窗口由 windui 自动注入。
pub fn pick_folder(title: &str) -> Option<PathBuf> {
    PickDialog::new().title(title).pick_folder()
}

/// 确认对话框（确定/取消）。返回是否点了"确定"。
pub fn confirm(owner: Option<HWND>, text: &str, caption: &str) -> bool {
    msgbox(owner, text, caption, MB_OKCANCEL | MB_ICONQUESTION) == IDOK
}

/// 错误提示。
pub fn error(owner: Option<HWND>, text: &str, caption: &str) {
    let _ = msgbox(owner, text, caption, MESSAGEBOX_STYLE(0) | MB_ICONERROR);
}

fn msgbox(
    owner: Option<HWND>,
    text: &str,
    caption: &str,
    style: MESSAGEBOX_STYLE,
) -> MESSAGEBOX_RESULT {
    let t = wide(text);
    let c = wide(caption);
    unsafe { MessageBoxW(owner, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), style) }
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
