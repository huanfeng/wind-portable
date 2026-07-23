//! 便携布局探测（对应 C# `PortableConfig`）。
//!
//! 在 launcher 所在目录及其上级、当前工作目录及其上级中，寻找服务程序，定位整套便携布局：
//! 服务/设置 exe、TSF DLL（x64/x86）、userdata 目录、便携标记、图标。

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::variant::{self, Variant};

/// 探测到的便携布局。
#[derive(Debug, Clone)]
pub struct PortableConfig {
    /// 便携包根目录（服务 exe 所在的逻辑根）。
    pub root_dir: PathBuf,
    /// 便携数据目录（root/userdata）。
    pub userdata_dir: PathBuf,
    /// 便携标记文件路径（root/wind_portable_mode）。
    pub portable_marker: PathBuf,
    /// 服务可执行文件绝对路径。
    pub service_exe: PathBuf,
    /// 设置程序绝对路径（可能尚不存在，取约定位置）。
    pub setting_exe: PathBuf,
    /// TSF x64 DLL 路径（不存在则 None）。
    pub tsf_dll: Option<PathBuf>,
    /// TSF x86 DLL 路径（不存在则 None）。
    pub tsf_dll_x86: Option<PathBuf>,
    /// 托盘/窗口图标路径（不存在则 None）。第二阶段接入真实 .ico 图标时使用；
    /// 当前托盘用纯色图标。
    #[allow(dead_code)]
    pub icon_path: Option<PathBuf>,
}

/// 用当前进程位置 + 工作目录探测便携布局。
///
/// **不因"位于系统保护目录"而失败**：那里的文件同样是齐全的一套便携包，探测不该谎称
/// 找不到。保护目录改由 [`crate::registration::installed_conflict`] 报为冲突——便携模式
/// 禁用、但"把这套文件复制到别处"的逃生口保持可用（探测失败会连 `ServiceManager` 一起
/// 丢掉，复制部署便无源可用）。探测失败只保留一种含义：**真的没找到服务 exe**。
pub fn detect(variant: &Variant) -> Result<PortableConfig> {
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(Path::parent).map(Path::to_path_buf);
    let wd = std::env::current_dir().ok();
    let candidates = candidate_roots(exe_dir.as_deref(), wd.as_deref());
    detect_from(variant, &candidates)
}

/// 纯逻辑：在给定候选根目录中按序探测，命中第一个含服务 exe 的根。
pub fn detect_from(variant: &Variant, candidates: &[PathBuf]) -> Result<PortableConfig> {
    for root in candidates {
        let Some(svc) = first_existing(&build_candidates(root, &variant.service_name)) else {
            continue;
        };
        let setting = first_existing(&build_candidates(root, &variant.setting_name))
            .unwrap_or_else(|| root.join(&variant.setting_name));
        let tsf_dll = first_existing(&build_candidates(root, &variant.dll_name));
        let tsf_dll_x86 = first_existing(&build_candidates(root, &variant.dll_name_x86));
        let icon_path = first_existing(&[
            root.join("wind_portable")
                .join("res")
                .join("wind_input_portable.ico"),
            root.join("res").join("wind_input_portable.ico"),
            root.join("wind_tsf").join("res").join("wind_input.ico"),
        ]);
        let userdata_dir = variant::portable_data_dir(root);
        return Ok(PortableConfig {
            root_dir: root.clone(),
            userdata_dir,
            portable_marker: root.join(variant::PORTABLE_MARKER_NAME),
            service_exe: svc,
            setting_exe: setting,
            tsf_dll,
            tsf_dll_x86,
            icon_path,
        });
    }
    bail!(
        "未找到 {}，请先构建主服务或将 launcher 放到打包目录中",
        variant.service_name
    );
}

/// 部署源目录探测：找到第一个含服务 exe 的候选根（用于"复制部署"功能）。
/// 第二阶段（zip/目录部署）接入；当前未使用。
#[allow(dead_code)]
pub fn find_deploy_source_dir(variant: &Variant) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(Path::parent).map(Path::to_path_buf);
    let wd = std::env::current_dir().ok();
    let candidates = candidate_roots(exe_dir.as_deref(), wd.as_deref());
    candidates
        .into_iter()
        .find(|root| first_existing(&build_candidates(root, &variant.service_name)).is_some())
}

/// 候选根目录：exe 目录、其上级、cwd、其上级（去重、规整为绝对路径）。
fn candidate_roots(exe_dir: Option<&Path>, wd: Option<&Path>) -> Vec<PathBuf> {
    let mut raw: Vec<PathBuf> = Vec::new();
    let mut push = |p: Option<&Path>| {
        if let Some(p) = p {
            raw.push(p.to_path_buf());
            if let Some(parent) = p.parent() {
                raw.push(parent.to_path_buf());
            }
        }
    };
    push(exe_dir);
    push(wd);
    unique_paths(&raw)
}

/// 在 root、root/build、root/build_dev 三处找文件名 `name`。
fn build_candidates(root: &Path, name: &str) -> Vec<PathBuf> {
    vec![
        root.join(name),
        root.join("build").join(name),
        root.join("build_dev").join(name),
    ]
}

/// 返回首个存在的文件路径。
fn first_existing(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| p.is_file()).cloned()
}

/// 去重（按路径串大小写不敏感比较），保持顺序。
///
/// 不做 `fs::canonicalize`：Windows 上它会返回 `\\?\` verbatim 前缀，破坏后续保护目录
/// 前缀匹配与注册表 DLL 路径比较（C# 用的是不带前缀的 `Path.GetFullPath`）。候选目录均来自
/// `current_exe`/`current_dir` 及其 `parent`，本就是干净的绝对路径。
fn unique_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        let key = p.to_string_lossy().to_lowercase();
        if !seen.contains(&key) {
            seen.push(key);
            out.push(p.clone());
        }
    }
    out
}

/// 是否位于系统保护目录（Program Files / Windows 等）。
pub fn is_protected_dir(dir: &Path) -> bool {
    let prefixes: Vec<String> = [
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "SystemRoot",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok())
    .filter(|v| !v.is_empty())
    .collect();
    is_protected_under(dir, &prefixes)
}

/// 纯逻辑：`dir` 是否在任一前缀目录之下。
///
/// 不做 `fs::canonicalize`（同 `unique_paths` 的理由，避免 Windows `\\?\` 前缀）；`dir`
/// 来自 `current_exe` 的 parent，已是绝对路径。
fn is_protected_under(dir: &Path, prefixes: &[String]) -> bool {
    let lower = dir.to_string_lossy().to_lowercase();
    prefixes.iter().any(|p| {
        let pfx = p.to_lowercase();
        let pfx = pfx.trim_end_matches(['\\', '/']);
        lower.starts_with(&format!("{pfx}\\")) || lower.starts_with(&format!("{pfx}/"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d =
            std::env::temp_dir().join(format!("wind_portable_layout_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn detect_finds_service_in_root() {
        let v = Variant::new(false);
        let root = tempdir();
        std::fs::write(root.join("wind_input.exe"), b"x").unwrap();
        std::fs::write(root.join("wind_tsf.dll"), b"x").unwrap();
        let cfg = detect_from(&v, &[root.clone()]).unwrap();
        assert_eq!(cfg.service_exe, root.join("wind_input.exe"));
        assert_eq!(cfg.tsf_dll, Some(root.join("wind_tsf.dll")));
        assert_eq!(cfg.tsf_dll_x86, None);
        assert_eq!(cfg.userdata_dir, root.join("userdata"));
        assert_eq!(cfg.portable_marker, root.join("wind_portable_mode"));
        // setting 不存在 → 取约定位置。
        assert_eq!(cfg.setting_exe, root.join("wind_setting.exe"));
    }

    #[test]
    fn detect_finds_service_in_build_subdir() {
        let v = Variant::new(false);
        let root = tempdir();
        std::fs::create_dir_all(root.join("build_dev")).unwrap();
        std::fs::write(root.join("build_dev").join("wind_input.exe"), b"x").unwrap();
        let cfg = detect_from(&v, &[root.clone()]).unwrap();
        assert_eq!(
            cfg.service_exe,
            root.join("build_dev").join("wind_input.exe")
        );
        assert_eq!(cfg.root_dir, root);
    }

    /// 位于系统保护目录**不**让探测失败。
    ///
    /// 回归：保护目录曾在 `detect` 开头直接 `bail!`，导致安装版（默认装在 Program Files）
    /// 里连 `PortableConfig` 都构造不出来 → `ServiceManager` 为 None → 「复制部署」按钮
    /// 被 `desired_enables` 的 detect_error 分支全灰，用户无法把安装版复制出去当便携用。
    /// 保护目录现由 `registration::installed_conflict` 报为冲突（便携功能禁用、部署放行）。
    #[test]
    fn detect_succeeds_inside_protected_dir() {
        let v = Variant::new(false);
        let root = tempdir();
        std::fs::write(root.join("wind_input.exe"), b"x").unwrap();
        // 把 root 自身当作"保护目录前缀"，确认判定确实命中……
        let prefixes = vec![root.to_string_lossy().to_string()];
        assert!(is_protected_under(&root.join("sub"), &prefixes));
        // ……而探测层照常给出完整布局，不因此失败。
        let cfg = detect_from(&v, std::slice::from_ref(&root)).unwrap();
        assert_eq!(cfg.root_dir, root);
    }

    #[test]
    fn detect_errors_when_no_service() {
        let v = Variant::new(false);
        let root = tempdir();
        let err = detect_from(&v, &[root]).unwrap_err();
        assert!(err.to_string().contains("wind_input.exe"));
    }

    #[test]
    fn dev_variant_finds_dev_service() {
        let v = Variant::new(true);
        let root = tempdir();
        std::fs::write(root.join("wind_input_dev.exe"), b"x").unwrap();
        let cfg = detect_from(&v, &[root.clone()]).unwrap();
        assert_eq!(cfg.service_exe, root.join("wind_input_dev.exe"));
    }

    #[test]
    fn protected_under_matches_prefix() {
        let prefixes = vec![r"C:\Program Files".to_string()];
        // 直接构造路径串比较逻辑（不依赖真实文件系统）。
        assert!(is_protected_under(
            Path::new(r"C:\Program Files\Wind"),
            &prefixes
        ));
        assert!(!is_protected_under(Path::new(r"D:\Apps\Wind"), &prefixes));
        // 前缀的兄弟目录不应误判。
        assert!(!is_protected_under(
            Path::new(r"C:\Program FilesX\Wind"),
            &prefixes
        ));
    }
}
