//! TSF 注册/注销与冲突检测（对应 C# `RegistrationManager`）。Windows 专用。
//!
//! 注册：icacls 授权 ALL_APP_PACKAGES → regsvr32 注册 DLL（x64 必需、x86 可选）→
//! InstallLayoutOrTip 安装输入法 profile。非管理员时经 ShellExecuteEx `runas` 自我提权，
//! 以 `-elevate-register` 重新拉起执行 `register_direct`。

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
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
use crate::variant::{has_portable_marker_in, Variant};

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

/// TSF DLL 的系统目录落点：`%SystemRoot%\System32\IME\<app_name>\<dll>`
/// （x86 → `SysWOW64\IME\<app_name>\`）。布局对齐 inbox IME 与安装版。
///
/// 为什么便携模式也要往系统目录放：开启 Trusted Mode 的游戏（CS2 等）按**加载路径**
/// 决定放不放行 in-proc DLL，便携目录里的副本连加载都会被拒。便携模式本就需要管理员
/// 权限、本就往 HKLM 写 COM 注册，「不碰系统目录」并不是它与安装版真正的分界线；
/// 两边行为一致，才不会出现「安装版游戏里能打字、便携版不能」这种没道理的差别。
fn system_dll_path(variant: &Variant, x86: bool) -> PathBuf {
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let root = if x86 { "SysWOW64" } else { "System32" };
    let name = if x86 {
        &variant.dll_name_x86
    } else {
        &variant.dll_name
    };
    Path::new(&sysroot)
        .join(root)
        .join("IME")
        .join(variant.app_name)
        .join(name)
}

/// 应用注册表键 `HKLM\Software\<app_name>`——存 `InstallDir`，与安装版同一个键。
fn app_reg_subkey(variant: &Variant) -> String {
    format!(r"Software\{}", variant.app_name)
}

/// 读回 `InstallDir`（当前持有注册的那个目录）。
fn registered_install_dir(variant: &Variant) -> Option<String> {
    crate::reg::read_string(
        crate::reg::HKEY_LOCAL_MACHINE,
        &app_reg_subkey(variant),
        "InstallDir",
    )
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// 复制到系统目录 → 授予 AppContainer 读权限 → 对**系统副本** regsvr32。
///
/// 授权与注册都针对系统副本：`DllRegisterServer` 用 `GetModuleFileName` 取被加载模块
/// 的路径写进 `InprocServer32`，对哪个副本跑 regsvr32 就指向哪个副本。
fn deploy_and_register(src: &Path, dst: &Path, x86: bool) -> Result<()> {
    copy_to_system_dir(src, dst)?;
    grant_app_packages_access(dst);
    regsvr32(dst, x86, false)
}

/// 复制到系统目录；旧副本被加载锁住时改名让路再复制。
///
/// ⚠️ **必须让路，不能直接 `fs::copy` 了事**：TSF DLL 是 in-proc 常驻的，系统目录里的
/// 旧副本只要还有宿主进程加载着就无法覆盖（NTFS 允许改名在用文件，却不允许覆盖它）
/// ——宿主不重启就一直锁着旧代，这是**常态而非异常**。
///
/// 曾漏过一次：安装器（`copy_to_system_dir`）与 `dev.ps1`（`Copy-Replace`）都有同款
/// 让路，唯独便携这条路径直接用了 `fs::copy`，一撞上就整个注册失败，而用户看到的
/// 只是提权子进程的退出码 → 报「提权失败」，指向完全错误的方向。
fn copy_to_system_dir(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow!("创建系统目录失败 {}: {e}", parent.display()))?;
        // 顺带收掉历次让路的残留（`*.old_<纳秒>`），否则系统目录里会一直累积。
        crate::deploy::clean_old_files(parent);
    }

    let Err(first) = std::fs::copy(src, dst) else {
        return Ok(());
    };
    if !dst.exists() {
        // 目标不存在却失败 → 不是被占用，是权限/磁盘一类的真错误，原样上报。
        return Err(anyhow!("复制到系统目录失败 {}: {first}", dst.display()));
    }

    let stash = crate::deploy::suffix_name(dst, "old");
    std::fs::rename(dst, &stash)
        .map_err(|e| anyhow!("旧副本被锁定且改名让路失败 {}: {e}", dst.display()))?;
    std::fs::copy(src, dst)
        .map(|_| ())
        .map_err(|e| anyhow!("让路后复制仍失败 {}: {e}", dst.display()))
}

/// 系统目录里那份副本是不是**本便携实例**部署的。
///
/// ⚠️ 系统副本路径对安装版与所有便携实例**完全相同**（`System32\IME\<app_name>\`），
/// 反注册/删除前必须先问这一句——无条件动手会把安装版的副本一并删掉，那边的输入法
/// 就废了。凭据是 `InstallDir` 指回本便携目录：谁最后部署，它就指向谁。
fn owns_system_deployment(cfg: &PortableConfig, variant: &Variant) -> bool {
    registered_install_dir(variant)
        .is_some_and(|dir| same_path(&dir, &cfg.root_dir.to_string_lossy()))
}

/// 已提权后的直接注册。
///
/// 两条路径由 `cfg.system_deploy`（`system_deploy` 标记文件）决定，**默认走就地注册**：
/// `System32\IME\` 与 `HKLM\Software\<app>\InstallDir` 都是与安装版共用的落点，
/// 同机共存时会互相覆盖，默认不碰即零冲突。
pub fn register_direct(cfg: &PortableConfig, variant: &Variant) -> Result<()> {
    let dll = cfg
        .tsf_dll
        .as_deref()
        .ok_or_else(|| anyhow!("未找到 TSF DLL，请先构建 {}", variant.dll_name))?;

    if cfg.system_deploy {
        // 便携目录回指。DLL 搬进系统目录后 `GetModuleFileName` 只能取到系统副本路径，
        // 服务拉起与便携标记检测都改读这个键（读端 wind_tsf 的 `_ResolveAppBaseDir`）。
        // 必须写在 regsvr32 之前：注册一完成宿主就可能加载 DLL，那时键还不在就会走空。
        // 它同时是本实例对系统副本的所有权凭据——系统副本各实例路径相同，靠路径已分不出
        // 是谁注册的（见 [`owns_system_deployment`]）。
        if !crate::reg::set_string(
            crate::reg::HKEY_LOCAL_MACHINE,
            &app_reg_subkey(variant),
            "InstallDir",
            &cfg.root_dir.to_string_lossy(),
        ) {
            bail!("写入 InstallDir 失败（需要管理员权限）");
        }

        deploy_and_register(dll, &system_dll_path(variant, false), false)?;

        if let Some(x86) = cfg.tsf_dll_x86.as_deref() {
            // x86 失败不致命（部分系统无 WOW64）
            let _ = deploy_and_register(x86, &system_dll_path(variant, true), true);
        }
    } else {
        // 默认：就地注册。DLL 留在便携目录，`GetModuleFileName` 推得出便携根，
        // wind_tsf 的 `_ResolveAppBaseDir` 回退到模块路径即可定位，无需写 InstallDir。
        grant_app_packages_access(dll);
        regsvr32(dll, false, false)?;

        if let Some(x86) = cfg.tsf_dll_x86.as_deref() {
            grant_app_packages_access(x86);
            let _ = regsvr32(x86, true, false); // x86 失败不致命（部分系统无 WOW64）
        }
    }

    if !install_layout_or_tip(variant.profile_str, 0) {
        bail!("InstallLayoutOrTip 失败");
    }
    Ok(())
}

/// 已提权后的直接注销（尽力而为，单步失败不阻断后续）。
pub fn unregister_direct(cfg: &PortableConfig, variant: &Variant) {
    let _ = install_layout_or_tip(variant.profile_str, ILOT_UNINSTALL);

    // 系统副本：**只动本实例部署的那份**。路径与安装版完全相同，无条件删会废掉安装版。
    // 判据用 owns_system_deployment 而不是 cfg.system_deploy：用户可能注册后才关掉开关，
    // 那时标记已没了、副本却还在，照 cfg 判就会漏清。
    if owns_system_deployment(cfg, variant) {
        for x86 in [true, false] {
            let sys = system_dll_path(variant, x86);
            if sys.is_file() {
                let _ = regsvr32(&sys, x86, true);
                let _ = std::fs::remove_file(&sys);
            }
        }
        // 收掉自建子目录。`remove_dir` 只删空目录——另一架构的副本还在（或删不掉）时
        // 自然失败，正是需要的语义。
        for x86 in [true, false] {
            if let Some(parent) = system_dll_path(variant, x86).parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
        let _ = crate::reg::delete_value(
            crate::reg::HKEY_LOCAL_MACHINE,
            &app_reg_subkey(variant),
            "InstallDir",
        );
    }

    // 就地注册的副本（默认路径，以及存量便携包）照常反注册一次，否则那条 CLSID 会
    // 滞留、且指向一个随后可能被删掉的路径。
    if let Some(x86) = cfg.tsf_dll_x86.as_deref() {
        let _ = regsvr32(x86, true, true);
    }
    if let Some(dll) = cfg.tsf_dll.as_deref() {
        let _ = regsvr32(dll, false, true);
    }
}

/// 本便携实例是否持有当前注册。
///
/// 两种形态分开判：
/// - **就地注册**（默认）：便携目录里的 DLL 路径本身唯一，直接比路径即可。
/// - **系统目录部署**：⚠️ 不能比路径——同一变体的所有实例（含安装版）算出的系统副本
///   路径**完全相同**，比了等于恒真。所有权只能由 `InstallDir` 判定，见
///   [`owns_system_deployment`]。
pub fn is_registered(cfg: &PortableConfig, variant: &Variant) -> bool {
    let Some(reg_path) = registered_dll_path(variant) else {
        return false;
    };
    let Some(dll) = cfg.tsf_dll.as_deref() else {
        return false;
    };

    let sys = system_dll_path(variant, false);
    if same_path(&reg_path, &sys.to_string_lossy()) {
        return owns_system_deployment(cfg, variant);
    }

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
    // 2. 当前目录在系统保护目录下（Program Files / Windows）。安装版通常两条都命中，
    //    由上一条给出更具体的提示；这条兜住"手工把便携包放进 Program Files"的情形。
    if crate::layout::is_protected_dir(&cfg.root_dir) {
        return Some(
            "当前位于系统保护目录，便携模式不可用。如需使用便携模式，请将文件复制到其他目录运行。"
                .to_string(),
        );
    }
    // 3. 其他位置注册了 DLL？
    let reg_path = registered_dll_path(variant)?;
    cfg.tsf_dll.as_deref()?; // 没有 DLL 就谈不上冲突（注册本身会失败）
    if is_registered(cfg, variant) {
        return None; // 当前注册就是本实例做的
    }
    // 注册文件已不存在 → 残留注册，可安全接管。
    if !Path::new(&reg_path).is_file() {
        return None;
    }
    // 不同位置的 DLL 已注册，判断来源目录。
    //
    // ⚠️ 注册指向系统副本时，来源目录只能从 `InstallDir` 取——系统副本旁边不可能有
    // 便携标记，照旧看「注册路径的同级目录」会把每一个便携实例都误判成安装版。
    let owner_dir = if same_path(
        &reg_path,
        &system_dll_path(variant, false).to_string_lossy(),
    ) {
        registered_install_dir(variant).map(PathBuf::from)
    } else {
        Path::new(&reg_path).parent().map(Path::to_path_buf)
    };
    if owner_dir.as_deref().is_some_and(has_portable_marker_in) {
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
    if crate::layout::is_protected_dir(&cfg.root_dir) {
        return Some(cfg.root_dir.to_string_lossy().into());
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
