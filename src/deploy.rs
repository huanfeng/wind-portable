//! 便携包部署与在线更新（对应 C# `DeployManager`）。
//!
//! 平台无关逻辑（std::fs + zip 纯 Rust 后端），可在 Linux 单测。

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};

use crate::variant::Variant;

/// 校验更新包：必须含服务 exe 与 TSF DLL（按文件名，大小写不敏感）。
pub fn validate_zip(zip_path: &Path, variant: &Variant) -> Result<()> {
    let file = fs::File::open(zip_path).map_err(|e| anyhow!("打开 ZIP 失败: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| anyhow!("读取 ZIP 失败: {e}"))?;

    let mut names: Vec<String> = Vec::new();
    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|e| anyhow!("读取 ZIP 条目失败: {e}"))?;
        if let Some(name) = Path::new(entry.name()).file_name() {
            names.push(name.to_string_lossy().to_lowercase());
        }
    }
    let required = [variant.service_name.as_str(), variant.dll_name.as_str()];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|r| !names.contains(&r.to_lowercase()))
        .collect();
    if !missing.is_empty() {
        bail!("ZIP 缺少必要文件: {}", missing.join(", "));
    }
    Ok(())
}

/// 从 ZIP 部署/更新到目标目录。返回 `needs_restart`（是否替换了正在运行的自身 exe）。
///
/// **原子替换**：每个文件先完整写到临时文件 `*.new_<n>`，再 `rename` 就位——写入中途失败
/// 时目标文件保持原样，绝不会留下截断/缺失的 exe。若目标被占用（运行中 exe / 已加载 DLL，
/// 不可覆盖但可改名），先把旧文件改名 `*.old_<n>` 再放新文件。残留由 [`clean_old_files`] 清理。
pub fn deploy_from_zip(zip_path: &Path, target_dir: &Path) -> Result<bool> {
    let self_exe = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_lowercase());
    let mut needs_restart = false;

    let file = fs::File::open(zip_path).map_err(|e| anyhow!("打开 ZIP 失败: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| anyhow!("读取 ZIP 失败: {e}"))?;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| anyhow!("读取 ZIP 条目失败: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        // enclosed_name 防止 `../` 路径穿越。
        let rel = entry
            .enclosed_name()
            .ok_or_else(|| anyhow!("ZIP 含非法条目路径"))?;
        let dst = target_dir.join(&rel);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|e| anyhow!("创建目录失败: {e}"))?;
        }
        let is_self = self_exe.as_deref() == Some(dst.to_string_lossy().to_lowercase().as_str());

        // 1. 先完整写到临时文件（失败时 dst 不受影响）。
        let tmp = suffix_name(&dst, "new");
        extract_to(&mut entry, &tmp).map_err(|e| anyhow!("解压失败: {e}"))?;

        // 2. 原子就位；若目标被占用无法覆盖，先把旧文件改名再放新文件。
        if fs::rename(&tmp, &dst).is_err() {
            let _ = fs::rename(&dst, suffix_name(&dst, "old"));
            if let Err(e) = fs::rename(&tmp, &dst) {
                let _ = fs::remove_file(&tmp);
                bail!("替换文件失败 {}: {e}", dst.display());
            }
        }
        if is_self {
            needs_restart = true;
        }
    }
    Ok(needs_restart)
}

/// 从目录复制部署到目标目录（跳过 `userdata/` 与安装版专属文件，见 [`skip_in_copy`]）。
pub fn deploy_from_directory(src_dir: &Path, target_dir: &Path) -> Result<()> {
    if same_dir(src_dir, target_dir) {
        bail!("源目录与目标目录相同");
    }
    let mut files = Vec::new();
    collect_files(src_dir, &mut files).map_err(|e| anyhow!("枚举源文件失败: {e}"))?;
    for src in files {
        let rel = src.strip_prefix(src_dir).unwrap_or(&src);
        if skip_in_copy(rel) {
            continue;
        }
        let dst = target_dir.join(rel);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|e| anyhow!("创建目录失败: {e}"))?;
        }
        fs::copy(&src, &dst).map_err(|e| anyhow!("复制 {} 失败: {e}", src.display()))?;
    }
    Ok(())
}

/// 清理本模块生成的替换残留：`*.old_<数字>`、`*.new_<数字>`、`*.bak`。
/// 用精确后缀（数字）而非 `.old_` 子串，避免误删形如 `notes.old_draft.txt` 的用户文件。
pub fn clean_old_files(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if is_backup_name(&name) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// 是否为本模块生成的替换残留文件名。
fn is_backup_name(name: &str) -> bool {
    if name.ends_with(".bak") {
        return true;
    }
    for marker in [".old_", ".new_"] {
        if let Some(pos) = name.rfind(marker) {
            let suffix = &name[pos + marker.len()..];
            if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
                return true;
            }
        }
    }
    false
}

// ── 内部 ──

/// 复制部署时应跳过的相对路径。
///
/// - `userdata/`：用户数据不随便携包分发。
/// - 根级 `uninstall.exe`：安装版专属。它若被带到目标目录，会让目标在便携运行时被
///   [`crate::registration`] 的 `is_installed_directory` 误判为"安装目录"而禁用便携模式
///   ——"把安装版复制到 U 盘当便携用"就是栽在这里：复制看似成功，到了别处却打不开。
///   仅拦根级（`rel` 无子层级），避免误伤用户目录里同名文件。
fn skip_in_copy(rel: &Path) -> bool {
    let mut comps = rel.components();
    let Some(first) = comps.next() else {
        return false;
    };
    let first = first.as_os_str().to_string_lossy();
    if first.eq_ignore_ascii_case("userdata") {
        return true; // userdata 整个目录（含所有子层级）
    }
    comps.next().is_none() && first.eq_ignore_ascii_case("uninstall.exe")
}

fn extract_to<R: Read>(entry: &mut R, dst: &Path) -> io::Result<()> {
    let mut out = fs::File::create(dst)?;
    io::copy(entry, &mut out)?;
    Ok(())
}

/// `<path>.<kind>_<纳秒>`（kind = old/new）：避免与现存文件冲突（不依赖随机数）。
fn suffix_name(p: &Path, kind: &str) -> PathBuf {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!("{}.{kind}_{n}", p.display()))
}

/// 两个目录是否指向同一位置（大小写不敏感；canonicalize 失败时退回原样比较）。
pub fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// 递归收集 `dir` 下所有文件路径。
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d =
            std::env::temp_dir().join(format!("wind_portable_deploy_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn deploy_from_directory_skips_userdata() {
        let src = tempdir();
        let dst = tempdir();
        fs::write(src.join("wind_input.exe"), b"exe").unwrap();
        fs::create_dir_all(src.join("data")).unwrap();
        fs::write(src.join("data").join("config.toml"), b"cfg").unwrap();
        fs::create_dir_all(src.join("userdata").join("logs")).unwrap();
        fs::write(src.join("userdata").join("db.redb"), b"user").unwrap();

        deploy_from_directory(&src, &dst).unwrap();

        assert!(dst.join("wind_input.exe").is_file());
        assert!(dst.join("data").join("config.toml").is_file());
        // userdata 被跳过。
        assert!(!dst.join("userdata").exists());
    }

    #[test]
    fn deploy_from_directory_skips_uninstaller() {
        // 场景：从安装目录复制到别处当便携用。安装版专属的 uninstall.exe 必须留下，
        // 否则目标会被 is_installed_directory 误判为安装目录、便携模式失效。
        let src = tempdir();
        let dst = tempdir();
        fs::write(src.join("wind_input.exe"), b"exe").unwrap();
        fs::write(src.join("uninstall.exe"), b"unins").unwrap();
        // 子目录下的同名文件不受影响（仅拦根级）。
        fs::create_dir_all(src.join("tools")).unwrap();
        fs::write(src.join("tools").join("uninstall.exe"), b"keep").unwrap();

        deploy_from_directory(&src, &dst).unwrap();

        assert!(dst.join("wind_input.exe").is_file());
        assert!(!dst.join("uninstall.exe").exists(), "根级卸载器应被跳过");
        assert!(
            dst.join("tools").join("uninstall.exe").is_file(),
            "子目录同名文件应照常复制"
        );
    }

    #[test]
    fn deploy_from_directory_rejects_same() {
        let d = tempdir();
        assert!(deploy_from_directory(&d, &d).is_err());
    }

    #[test]
    fn clean_old_files_removes_backups() {
        let d = tempdir();
        fs::write(d.join("wind_input.exe.old_123"), b"x").unwrap();
        fs::write(d.join("wind_tsf.dll.new_456"), b"x").unwrap();
        fs::write(d.join("config.bak"), b"x").unwrap();
        fs::write(d.join("keep.exe"), b"x").unwrap();
        // 非数字后缀的用户文件不应被误删。
        fs::write(d.join("notes.old_draft.txt"), b"x").unwrap();
        clean_old_files(&d);
        assert!(!d.join("wind_input.exe.old_123").exists());
        assert!(!d.join("wind_tsf.dll.new_456").exists());
        assert!(!d.join("config.bak").exists());
        assert!(d.join("keep.exe").is_file());
        assert!(d.join("notes.old_draft.txt").is_file());
    }

    #[test]
    fn validate_zip_detects_missing() {
        let d = tempdir();
        let zip_path = d.join("pkg.zip");
        // 写一个只含 readme 的 zip。
        let f = fs::File::create(&zip_path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        w.start_file("readme.txt", opts).unwrap();
        use std::io::Write;
        w.write_all(b"hi").unwrap();
        w.finish().unwrap();

        let v = Variant::new(false);
        let err = validate_zip(&zip_path, &v).unwrap_err();
        assert!(err.to_string().contains("wind_input.exe"));
    }

    #[test]
    fn validate_and_deploy_zip_roundtrip() {
        let d = tempdir();
        let zip_path = d.join("pkg.zip");
        {
            let f = fs::File::create(&zip_path).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            use std::io::Write;
            for name in ["wind_input.exe", "wind_tsf.dll", "data/config.toml"] {
                w.start_file(name, opts).unwrap();
                w.write_all(b"content").unwrap();
            }
            w.finish().unwrap();
        }
        let v = Variant::new(false);
        validate_zip(&zip_path, &v).unwrap();

        let target = tempdir();
        let needs_restart = deploy_from_zip(&zip_path, &target).unwrap();
        assert!(!needs_restart); // 测试进程 exe 不在包内
        assert!(target.join("wind_input.exe").is_file());
        assert!(target.join("data").join("config.toml").is_file());
    }
}
