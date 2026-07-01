//! Win32 通用对话框：文件选择 / 目录选择 / 消息框（对应 C# OpenFileDialog /
//! FolderBrowserDialog / MessageBox）。**均在 UI 线程调用**（模态，自带消息循环）。Windows 专用。

use std::ffi::{c_void, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows::Win32::UI::Shell::{
    SHBrowseForFolderW, SHGetPathFromIDListW, BIF_RETURNONLYFSDIRS, BROWSEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MessageBoxW, IDOK, MB_ICONERROR, MB_ICONQUESTION, MB_OKCANCEL, MESSAGEBOX_RESULT,
    MESSAGEBOX_STYLE,
};

/// 按标题找启动器窗口（用作对话框 owner，保证模态）。
pub fn find_main_window(title: &str) -> Option<HWND> {
    let t = wide(title);
    unsafe { FindWindowW(PCWSTR::null(), PCWSTR(t.as_ptr())).ok() }.filter(|h| !h.is_invalid())
}

/// 选择 ZIP 文件。取消返回 None。
pub fn pick_zip(owner: Option<HWND>, title: &str) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    let filter = encode_filter(&["ZIP 压缩包 (*.zip)", "*.zip", "所有文件 (*.*)", "*.*"]);
    let title_w = wide(title);

    let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = owner.unwrap_or_default();
    ofn.lpstrFilter = PCWSTR(filter.as_ptr());
    ofn.lpstrFile = PWSTR(buf.as_mut_ptr());
    ofn.nMaxFile = buf.len() as u32;
    ofn.nFilterIndex = 1;
    ofn.lpstrTitle = PCWSTR(title_w.as_ptr());
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_EXPLORER | OFN_HIDEREADONLY;

    let ok = unsafe { GetOpenFileNameW(&mut ofn) };
    if ok.as_bool() {
        Some(PathBuf::from(OsString::from_wide(until_nul(&buf))))
    } else {
        None
    }
}

/// 选择目录。取消返回 None。
pub fn pick_folder(owner: Option<HWND>, title: &str) -> Option<PathBuf> {
    let title_w = wide(title);
    let mut display = vec![0u16; 260];

    let mut bi: BROWSEINFOW = unsafe { std::mem::zeroed() };
    bi.hwndOwner = owner.unwrap_or_default();
    bi.pszDisplayName = PWSTR(display.as_mut_ptr());
    bi.lpszTitle = PCWSTR(title_w.as_ptr());
    bi.ulFlags = BIF_RETURNONLYFSDIRS;

    unsafe {
        let pidl = SHBrowseForFolderW(&bi);
        if pidl.is_null() {
            return None;
        }
        let mut path = [0u16; 260];
        let got = SHGetPathFromIDListW(pidl, &mut path);
        CoTaskMemFree(Some(pidl as *const c_void));
        if got.as_bool() {
            Some(PathBuf::from(OsString::from_wide(until_nul(&path))))
        } else {
            None
        }
    }
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

/// 截到首个 NUL 之前。
fn until_nul(buf: &[u16]) -> &[u16] {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    &buf[..end]
}

/// 通用文件过滤器编码：`label\0pattern\0...\0\0`。
fn encode_filter(parts: &[&str]) -> Vec<u16> {
    let mut v = Vec::new();
    for p in parts {
        v.extend(p.encode_utf16());
        v.push(0);
    }
    v.push(0);
    v
}
