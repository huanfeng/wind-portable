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
use windows::Win32::UI::Controls::{
    TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TASKDIALOG_BUTTON, TASKDIALOG_FLAGS,
    TDCBF_CANCEL_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_USE_COMMAND_LINKS, TD_WARNING_ICON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MessageBoxW, IDNO, IDOK, IDYES, MB_ICONERROR, MB_ICONQUESTION, MB_OKCANCEL,
    MB_YESNOCANCEL, MESSAGEBOX_RESULT, MESSAGEBOX_STYLE,
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

/// 三态询问的结果（对应"是 / 否 / 取消"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    Cancel,
}

/// 三态询问（是/否/取消）。用于"顺带做某事"与"仅执行主动作"需要区分的场景——
/// 两态确认框会把"否"和"取消"压成同一个答案，语义丢失。
/// 关闭标题栏 × 等同"取消"（`MessageBoxW` 在 MB_YESNOCANCEL 下返回 IDCANCEL）。
pub fn ask(owner: Option<HWND>, text: &str, caption: &str) -> Answer {
    match msgbox(owner, text, caption, MB_YESNOCANCEL | MB_ICONQUESTION) {
        IDYES => Answer::Yes,
        IDNO => Answer::No,
        _ => Answer::Cancel,
    }
}

/// 命令链接三态对话框（TaskDialog）：主标题 + 两个大按钮（各含标题行与灰色说明行）+ 取消。
/// 比 `MessageBoxW` 的「是/否/取消」表意清楚得多——按钮文字直接写明后果，无需正文硬凑。
/// `opt1`/`opt2` 各为 `(按钮标题, 说明小字)`；返回 `Yes`=选项1、`No`=选项2、`Cancel`=取消/Esc/×。
///
/// TaskDialog 需要 comctl32 v6（由嵌入的应用清单保证）。万一调用失败（理论上不会），
/// 回退到 `ask` 的 `MessageBoxW` 版本，保证退出流程永远可用。
pub fn ask_commands(
    owner: Option<HWND>,
    title: &str,
    instruction: &str,
    opt1: (&str, &str),
    opt2: (&str, &str),
) -> Answer {
    /// 命令链接按钮 ID（任取，避开系统 ID）。
    const ID_OPT1: i32 = 101;
    const ID_OPT2: i32 = 102;

    // 命令链接文本：首行是标题，`\n` 之后是灰色说明小字（TaskDialog 约定）。
    let title_w = wide(title);
    let instr_w = wide(instruction);
    let btn1_w = wide(&format!("{}\n{}", opt1.0, opt1.1));
    let btn2_w = wide(&format!("{}\n{}", opt2.0, opt2.1));
    // 这些缓冲需在 TaskDialogIndirect 同步调用期间存活——全在本栈帧，OK。
    let buttons = [
        TASKDIALOG_BUTTON {
            nButtonID: ID_OPT1,
            pszButtonText: PCWSTR(btn1_w.as_ptr()),
        },
        TASKDIALOG_BUTTON {
            nButtonID: ID_OPT2,
            pszButtonText: PCWSTR(btn2_w.as_ptr()),
        },
    ];

    let cfg = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner.unwrap_or_default(),
        dwFlags: TASKDIALOG_FLAGS(TDF_USE_COMMAND_LINKS.0 | TDF_ALLOW_DIALOG_CANCELLATION.0),
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: PCWSTR(title_w.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_WARNING_ICON,
        },
        pszMainInstruction: PCWSTR(instr_w.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        // 初始焦点落在"仅执行主动作"（较安全的一项），回车不会误触发卸载。
        nDefaultButton: ID_OPT2,
        ..Default::default()
    };

    let mut pressed = 0i32;
    match unsafe { TaskDialogIndirect(&cfg, Some(&mut pressed), None, None) } {
        Ok(()) => match pressed {
            ID_OPT1 => Answer::Yes,
            ID_OPT2 => Answer::No,
            _ => Answer::Cancel, // IDCANCEL / Esc / ×
        },
        // comctl32 v6 缺失等极端情况：回退到经典三态框，语义等价。
        Err(_) => ask(
            owner,
            &format!(
                "{instruction}\n\n是——{}\n否——{}\n取消——不退出",
                opt1.0, opt2.0
            ),
            title,
        ),
    }
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
