//! 构建变体探测与常量。
//!
//! 与原 C# `BuildVariant` 一致：launcher 是**单一二进制**，运行时探测——若在 exe 同级或
//! 上级目录发现 `wind_input_dev.exe`（或 `build_dev/wind_input_dev.exe`），切换到开发版
//! 变体；否则发布版。开发版与发布版用**不同的** CLSID/Profile/管道/目录/Mutex，彼此隔离。

use std::path::{Path, PathBuf};

/// 便携模式标记文件名（放在便携包根目录，DLL 同级用于辨认便携实例）。
///
/// 与安装器清单 `[app] portable_marker` 及 wind-config 的 `PORTABLE_MARKER_NAME` **同名**
/// ——三处曾不一致，安装包便携模式装出的目录主程序不认，数据落回 `%APPDATA%`。
pub const PORTABLE_MARKER_NAME: &str = "portable_mode";

/// 旧标记文件名，**仅用于读取兼容**：存量便携包里是这个名字。新写入一律用
/// [`PORTABLE_MARKER_NAME`]，`ensure_layout` 会顺带补建新名完成迁移。
pub const LEGACY_PORTABLE_MARKER_NAME: &str = "wind_portable_mode";

/// 目录内是否存在便携标记（新名优先，旧名兼容）。
pub fn has_portable_marker_in(dir: &Path) -> bool {
    dir.join(PORTABLE_MARKER_NAME).is_file() || dir.join(LEGACY_PORTABLE_MARKER_NAME).is_file()
}
/// 便携数据目录名（相对便携包根目录）。
pub const PORTABLE_DATA_DIR: &str = "userdata";

// 发布版 GUID 串（与 wind_tsf Globals / C# BuildVariant 对齐）。
const CLSID_RELEASE: &str = "{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}";
const PROFILE_RELEASE: &str =
    "0804:{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}";
// 开发版 GUID 串。
const CLSID_DEV: &str = "{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}";
const PROFILE_DEV: &str =
    "0804:{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}{99C2DEB1-5C57-45A2-9C63-FB54B34FD90A}";

/// 变体下的全部名称/常量。`new(is_dev)` 一次性算出。
#[derive(Debug, Clone)]
pub struct Variant {
    pub is_dev: bool,
    /// 显示名（窗口标题、NSIS 卸载键）。
    pub display_name: &'static str,
    /// 数据目录名（%APPDATA%\<此名>）与自启注册表值名（与安装版一致）。
    pub app_name: &'static str,
    /// TSF CLSID（含花括号）。
    pub clsid: &'static str,
    /// InstallLayoutOrTip 的 profile 串。
    pub profile_str: &'static str,
    /// 服务可执行名，如 `wind_input.exe`。
    pub service_name: String,
    /// 设置程序可执行名，如 `wind_setting.exe`。
    pub setting_name: String,
    /// TSF x64 DLL 名，如 `wind_tsf.dll`。
    pub dll_name: String,
    /// TSF x86 DLL 名，如 `wind_tsf_x86.dll`。
    pub dll_name_x86: String,
    /// RPC 控制管道后缀（端点为 `\\.\pipe\wind_input{suffix}_ctrl`）。
    pub pipe_suffix: String,
    /// 单实例 Mutex 名。
    pub mutex_name: String,
}

impl Variant {
    /// 由 debug 标志构造全部常量。
    pub fn new(is_dev: bool) -> Self {
        let suffix = if is_dev { "_dev" } else { "" };
        Variant {
            is_dev,
            display_name: if is_dev {
                "清风输入法开发版"
            } else {
                "清风输入法"
            },
            app_name: if is_dev { "WindInputDev" } else { "WindInput" },
            clsid: if is_dev { CLSID_DEV } else { CLSID_RELEASE },
            profile_str: if is_dev { PROFILE_DEV } else { PROFILE_RELEASE },
            service_name: format!("wind_input{suffix}.exe"),
            setting_name: format!("wind_setting{suffix}.exe"),
            dll_name: format!("wind_tsf{suffix}.dll"),
            dll_name_x86: format!("wind_tsf_x86{suffix}.dll"),
            pipe_suffix: suffix.to_string(),
            mutex_name: format!(r"Local\WindPortableLauncher{suffix}"),
        }
    }

    /// 运行时探测当前进程应使用的变体。
    pub fn detect() -> Self {
        Variant::new(detect_dev())
    }
}

/// 探测当前 exe 旁是否存在开发版服务，决定是否走开发版变体。
fn detect_dev() -> bool {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            dirs.push(d.to_path_buf());
            if let Some(p) = d.parent() {
                dirs.push(p.to_path_buf());
            }
        }
    }
    detect_dev_in(&dirs)
}

/// 纯逻辑：给定候选目录集，判断是否存在 `wind_input_dev.exe`
/// （直接同级，或 `build_dev/` 子目录下）。
pub fn detect_dev_in(dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|dir| {
        dir.join("wind_input_dev.exe").is_file()
            || dir.join("build_dev").join("wind_input_dev.exe").is_file()
    })
}

/// 便携包根目录下的便携数据目录绝对路径。
pub fn portable_data_dir(root: &Path) -> PathBuf {
    root.join(PORTABLE_DATA_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_names() {
        let v = Variant::new(false);
        assert!(!v.is_dev);
        assert_eq!(v.pipe_suffix, "");
        assert_eq!(v.service_name, "wind_input.exe");
        assert_eq!(v.setting_name, "wind_setting.exe");
        assert_eq!(v.dll_name, "wind_tsf.dll");
        assert_eq!(v.dll_name_x86, "wind_tsf_x86.dll");
        assert_eq!(v.app_name, "WindInput");
        assert_eq!(v.mutex_name, r"Local\WindPortableLauncher");
        assert_eq!(v.clsid, CLSID_RELEASE);
    }

    #[test]
    fn dev_names() {
        let v = Variant::new(true);
        assert!(v.is_dev);
        assert_eq!(v.pipe_suffix, "_dev");
        assert_eq!(v.service_name, "wind_input_dev.exe");
        assert_eq!(v.dll_name_x86, "wind_tsf_x86_dev.dll");
        assert_eq!(v.app_name, "WindInputDev");
        assert_eq!(v.mutex_name, r"Local\WindPortableLauncher_dev");
        assert_eq!(v.clsid, CLSID_DEV);
        assert_eq!(v.display_name, "清风输入法开发版");
    }

    #[test]
    fn detect_dev_direct_and_build_subdir() {
        let tmp = tempdir();
        // 无文件 → 发布版。
        assert!(!detect_dev_in(&[tmp.clone()]));
        // 同级直接放 → 开发版。
        std::fs::write(tmp.join("wind_input_dev.exe"), b"x").unwrap();
        assert!(detect_dev_in(&[tmp.clone()]));
        std::fs::remove_file(tmp.join("wind_input_dev.exe")).unwrap();
        // build_dev 子目录 → 开发版。
        std::fs::create_dir_all(tmp.join("build_dev")).unwrap();
        std::fs::write(tmp.join("build_dev").join("wind_input_dev.exe"), b"x").unwrap();
        assert!(detect_dev_in(&[tmp.clone()]));
    }

    /// 极简临时目录（避免引入 tempfile 依赖）：用进程 id + 计数器命名。
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d =
            std::env::temp_dir().join(format!("wind_portable_test_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
