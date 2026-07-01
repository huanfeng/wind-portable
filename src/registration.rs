//! TSF 注册/注销与冲突检测（对应 C# `RegistrationManager`）。Windows 专用。
//!
//! 注册：icacls 授权 ALL_APP_PACKAGES → regsvr32 注册 DLL（x64 必需、x86 可选）→
//! InstallLayoutOrTip 安装输入法 profile。非管理员时经 ShellExecuteEx `runas` 自我提权，
//! 以 `-elevate-register` 重新拉起执行 `register_direct`。

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Result};
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};

use crate::layout::PortableConfig;
use crate::variant::{Variant, PORTABLE_MARKER_NAME};

/// 隐藏子进程控制台窗口（CREATE_NO_WINDOW）。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// InstallLayoutOrTip 卸载标志。
const ILOT_UNINSTALL: u32 = 0x0000_0001;
/// ALL_APPLICATION_PACKAGES SID（沙箱应用加载 IME 所需）。
const ALL_APP_PACKAGES_SID: &str = "*S-1-15-2-1:(RX)";

// ── 对外接口 ──

/// 注册输入法（管理员直接执行，否则提权）。
pub fn register(cfg: &PortableConfig, variant: &Variant) -> Result<()> {
    if cfg.tsf_dll.is_none() {
        bail!("未找到 TSF DLL，请先构建 {}", variant.dll_name);
    }
    if is_elevated() {
        register_direct(cfg, variant)
    } else {
        run_elevated("-elevate-register")
    }
}

/// 注销输入法（管理员直接执行，否则提权）。
pub fn unregister(cfg: &PortableConfig, variant: &Variant) -> Result<()> {
    if is_elevated() {
        unregister_direct(cfg, variant);
        Ok(())
    } else {
        run_elevated("-elevate-unregister")
    }
}

/// 已提权后的直接注册。
pub fn register_direct(cfg: &PortableConfig, variant: &Variant) -> Result<()> {
    let dll = cfg
        .tsf_dll
        .as_deref()
        .ok_or_else(|| anyhow!("未找到 TSF DLL，请先构建 {}", variant.dll_name))?;

    grant_app_packages_access(dll);
    regsvr32(dll, false, false)?;

    if let Some(x86) = cfg.tsf_dll_x86.as_deref() {
        grant_app_packages_access(x86);
        let _ = regsvr32(x86, true, false); // x86 失败不致命（部分系统无 WOW64）
    }

    if !install_layout_or_tip(variant.profile_str, 0) {
        bail!("InstallLayoutOrTip 失败");
    }
    Ok(())
}

/// 已提权后的直接注销（尽力而为，单步失败不阻断后续）。
pub fn unregister_direct(cfg: &PortableConfig, variant: &Variant) {
    let _ = install_layout_or_tip(variant.profile_str, ILOT_UNINSTALL);
    if let Some(x86) = cfg.tsf_dll_x86.as_deref() {
        let _ = regsvr32(x86, true, true);
    }
    if let Some(dll) = cfg.tsf_dll.as_deref() {
        let _ = regsvr32(dll, false, true);
    }
}

/// 本便携 DLL 是否已注册（注册的 CLSID InprocServer32 指向本 DLL）。
pub fn is_registered(cfg: &PortableConfig, variant: &Variant) -> bool {
    let Some(reg_path) = registered_dll_path(variant) else {
        return false;
    };
    let Some(dll) = cfg.tsf_dll.as_deref() else {
        return false;
    };
    same_path(&reg_path, &dll.to_string_lossy())
}

/// 冲突检测：返回 `Some(原因)` 表示便携模式不可用。
pub fn installed_conflict(
    cfg: &PortableConfig,
    variant: &Variant,
    service_running: bool,
) -> Option<String> {
    // 1. 当前目录是安装版目录。
    if is_installed_directory(&cfg.root_dir, variant) {
        return Some(
            "当前位于已安装目录，便携模式不可用。如需使用便携模式，请将文件复制到其他目录运行。"
                .to_string(),
        );
    }
    // 2. 其他位置注册了 DLL？
    let reg_path = registered_dll_path(variant)?;
    let dll = cfg.tsf_dll.as_deref()?;
    if same_path(&reg_path, &dll.to_string_lossy()) {
        return None;
    }
    // 注册文件已不存在 → 残留注册，可安全接管。
    if !Path::new(&reg_path).is_file() {
        return None;
    }
    // 不同位置的 DLL 已注册，判断来源。
    if has_portable_marker(&reg_path) {
        if !service_running {
            return None; // 残留便携注册，服务未运行，可接管。
        }
        return Some("检测到另一个便携版实例正在运行，请先停止该实例后再启动。".to_string());
    }
    Some("系统已注册其他位置的清风输入法，为避免覆盖现有注册信息，便携模式已禁用。".to_string())
}

/// 冲突位置（用于界面提示）。
pub fn installed_conflict_path(cfg: &PortableConfig, variant: &Variant) -> Option<String> {
    if is_installed_directory(&cfg.root_dir, variant) {
        return Some(
            nsis_install_location(variant).unwrap_or_else(|| cfg.root_dir.to_string_lossy().into()),
        );
    }
    registered_dll_path(variant)
}

// ── 内部实现 ──

/// 当前进程是否提权（Administrator 完整令牌）。
fn is_elevated() -> bool {
    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// 以管理员身份重新拉起自身执行 `args`（UAC 提权），等待其结束。
fn run_elevated(args: &str) -> Result<()> {
    let exe = std::env::current_exe()?;
    let verb = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let params = wide(args);
    unsafe {
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: 1, // SW_SHOWNORMAL
            ..Default::default()
        };
        ShellExecuteExW(&mut info).map_err(|_| anyhow!("请求管理员权限失败或被取消"))?;
        if !info.hProcess.is_invalid() {
            let wait = WaitForSingleObject(info.hProcess, 30_000);
            // 读子进程退出码：-elevate-register/unregister 失败时 main 以 exit(1) 退出，
            // 据此把"提权后仍失败"如实反馈给调用方（C# 原版忽略退出码，此处加强）。
            let result = if wait == WAIT_TIMEOUT {
                Err(anyhow!("提权操作超时"))
            } else {
                let mut code = 0u32;
                if GetExitCodeProcess(info.hProcess, &mut code).is_ok() && code != 0 {
                    Err(anyhow!("提权操作失败（退出码 {code}）"))
                } else {
                    Ok(())
                }
            };
            let _ = CloseHandle(info.hProcess);
            return result;
        }
    }
    Ok(())
}

/// 动态加载 input.dll 调 InstallLayoutOrTip。返回是否成功。
fn install_layout_or_tip(profile: &str, flags: u32) -> bool {
    let lib_name = wide("input.dll");
    let profile_w = wide(profile);
    unsafe {
        let Ok(lib) = LoadLibraryW(PCWSTR(lib_name.as_ptr())) else {
            return false;
        };
        let proc = GetProcAddress(lib, PCSTR(c"InstallLayoutOrTip".as_ptr() as *const u8));
        let Some(proc) = proc else {
            return false;
        };
        type InstallFn = unsafe extern "system" fn(*const u16, u32) -> i32;
        let f: InstallFn = std::mem::transmute(proc);
        f(profile_w.as_ptr(), flags) != 0
    }
}

/// regsvr32 注册/注销 DLL。`x86` 用 SysWOW64 版本。
fn regsvr32(dll: &Path, x86: bool, unregister: bool) -> Result<()> {
    if !dll.is_file() {
        bail!("未找到 DLL: {}", dll.display());
    }
    let exe = if x86 {
        let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        Path::new(&sysroot).join("SysWOW64").join("regsvr32.exe")
    } else {
        Path::new("regsvr32.exe").to_path_buf()
    };
    let mut cmd = Command::new(exe);
    if unregister {
        cmd.arg("/u");
    }
    cmd.arg("/s").arg(dll).creation_flags(CREATE_NO_WINDOW);
    let status = cmd
        .status()
        .map_err(|e| anyhow!("regsvr32 启动失败 ({}): {e}", dll.display()))?;
    if !status.success() {
        bail!(
            "regsvr32 执行失败 ({}): 退出码 {:?}",
            dll.file_name().unwrap_or_default().to_string_lossy(),
            status.code()
        );
    }
    Ok(())
}

/// 授予 ALL_APPLICATION_PACKAGES 读取/执行权限（沙箱应用加载 IME 所需）。尽力而为。
fn grant_app_packages_access(dll: &Path) {
    let _ = Command::new("icacls")
        .arg(dll)
        .arg("/grant")
        .arg(ALL_APP_PACKAGES_SID)
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

/// 读注册的 CLSID InprocServer32 默认值（DLL 路径）。
fn registered_dll_path(variant: &Variant) -> Option<String> {
    let clsid = variant.clsid;
    let candidates = [
        (
            crate::reg::HKEY_CURRENT_USER,
            format!(r"Software\Classes\CLSID\{clsid}\InprocServer32"),
        ),
        (
            crate::reg::HKEY_LOCAL_MACHINE,
            format!(r"Software\Classes\CLSID\{clsid}\InprocServer32"),
        ),
        (
            crate::reg::HKEY_CLASSES_ROOT,
            format!(r"CLSID\{clsid}\InprocServer32"),
        ),
    ];
    for (root, path) in candidates {
        if let Some(v) = crate::reg::read_string(root, &path, "") {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// NSIS 安装位置（HKLM 卸载键 InstallLocation）。
fn nsis_install_location(variant: &Variant) -> Option<String> {
    let path = format!(
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{}",
        variant.display_name
    );
    crate::reg::read_string(crate::reg::HKEY_LOCAL_MACHINE, &path, "InstallLocation")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 目录是否为安装版目录（NSIS 注册位置匹配，或同目录有 uninstall.exe）。
fn is_installed_directory(root: &Path, variant: &Variant) -> bool {
    if let Some(install) = nsis_install_location(variant) {
        if same_path(&install, &root.to_string_lossy()) {
            return true;
        }
    }
    root.join("uninstall.exe").is_file()
}

/// DLL 同级是否存在便携标记文件（不向上遍历）。
fn has_portable_marker(dll_path: &str) -> bool {
    Path::new(dll_path)
        .parent()
        .map(|d| d.join(PORTABLE_MARKER_NAME).is_file())
        .unwrap_or(false)
}

/// 路径大小写不敏感比较：统一分隔符为 `\`、去尾分隔符、转小写后比较。
///
/// 不做 `fs::canonicalize`（避免 Windows `\\?\` 前缀）。两侧路径均为构造出的干净绝对路径
/// （注册表写入的 DLL 路径 + 由 current_exe 拼接的便携路径，见 layout.rs），故不展开 `.`/`..`；
/// 仅统一分隔符以容忍混用 `/` 与 `\`。
fn same_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

/// NUL 结尾宽字符串。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
