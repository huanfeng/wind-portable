//! 服务生命周期管理（对应 C# `ServiceManager`）。Windows 专用。
//!
//! 协调 RPC 探活、TSF 注册、自启注册表、便携目录布局与服务进程启停。

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::layout::PortableConfig;
use crate::variant::Variant;
use crate::{deploy, process, reg, registration, rpc};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// 服务管理器：持有探测到的布局与变体。
pub struct ServiceManager {
    pub cfg: PortableConfig,
    pub variant: Variant,
}

impl ServiceManager {
    pub fn new(cfg: PortableConfig, variant: Variant) -> Self {
        Self { cfg, variant }
    }

    /// 服务是否在运行（RPC 探活）。
    pub fn service_running(&self) -> bool {
        rpc::is_running(&self.variant)
    }

    /// 本便携 DLL 是否已注册。
    pub fn is_registered(&self) -> bool {
        registration::is_registered(&self.cfg, &self.variant)
    }

    /// 冲突原因（None=无冲突）。
    pub fn installed_conflict(&self, service_running: bool) -> Option<String> {
        registration::installed_conflict(&self.cfg, &self.variant, service_running)
    }

    /// 冲突位置（界面提示）。
    pub fn installed_conflict_path(&self) -> Option<String> {
        registration::installed_conflict_path(&self.cfg, &self.variant)
    }

    /// 启动服务：冲突检查 → 布局 → 注册 → 自启 → 拉起进程。
    pub fn start_service(&self) -> Result<()> {
        self.ensure_available("启动服务")?;

        if self.service_running() {
            // RPC 可用：若是我们自己的服务进程，已在运行直接返回。
            if process::exists_by_path(&self.cfg.service_exe) {
                return Ok(());
            }
            // 否则是占据管道的他处残留实例，先关闭。
            self.shutdown_stale_service();
        }

        self.ensure_portable_layout()?;
        self.clear_stopped_flag();

        if !self.is_registered() {
            registration::register(&self.cfg, &self.variant)?;
        }
        self.set_autostart(true);

        if !self.cfg.service_exe.is_file() {
            bail!("未找到服务程序: {}", self.cfg.service_exe.display());
        }
        let workdir = self
            .cfg
            .service_exe
            .parent()
            .ok_or_else(|| anyhow!("服务路径无父目录"))?;
        Command::new(&self.cfg.service_exe)
            .current_dir(workdir)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| anyhow!("启动服务进程失败: {e}"))?;
        Ok(())
    }

    /// 停止服务：守卫标志 → 优雅关闭/终止进程 → 移除自启 → 注销。返回此前是否运行或已注册。
    pub fn stop_service(&self) -> Result<bool> {
        self.ensure_available("停止服务")?;
        self.set_stopped_flag();

        let was_running = self.service_running();
        let was_registered = self.is_registered();
        // 用进程存在性（而非 RPC 探活）判定是否需要终止：RPC 管道可能瞬态不可用，
        // 但进程仍在；按进程扫描更可靠。
        let proc_alive = process::exists_by_path(&self.cfg.service_exe);

        if was_running || proc_alive {
            // 预留：先尝试 RPC 优雅关闭（当前服务端未实现→必失败，回退终止进程）。
            let mut graceful = false;
            if rpc::try_shutdown(&self.variant) {
                for _ in 0..6 {
                    sleep(Duration::from_millis(500));
                    if !process::exists_by_path(&self.cfg.service_exe) {
                        graceful = true;
                        break;
                    }
                }
            }
            if !graceful {
                // 不信任探活，无条件终止：先按全路径精确终止，未命中再按进程名兜底
                // （服务可能从其他工作目录拉起，路径写法略有差异）。
                if !process::terminate_by_path(&self.cfg.service_exe) {
                    if let Some(stem) = self.service_stem() {
                        process::terminate_by_name(&stem);
                    }
                }
                // 验证退出（最多 ~1.8s）。
                for _ in 0..6 {
                    if !process::exists_by_path(&self.cfg.service_exe) {
                        break;
                    }
                    sleep(Duration::from_millis(300));
                }
            }
        }

        self.set_autostart(false);

        if was_registered {
            registration::unregister(&self.cfg, &self.variant)?;
        }
        Ok(was_running || was_registered)
    }

    /// 服务 exe 文件名（不含扩展名），用于按名兜底终止。
    fn service_stem(&self) -> Option<String> {
        self.cfg
            .service_exe
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 打开设置程序。
    pub fn open_settings(&self) -> Result<()> {
        if !self.cfg.setting_exe.is_file() {
            bail!("未找到设置程序: {}", self.cfg.setting_exe.display());
        }
        let workdir = self.cfg.setting_exe.parent().unwrap_or(Path::new("."));
        Command::new(&self.cfg.setting_exe)
            .current_dir(workdir)
            .spawn()
            .map_err(|e| anyhow!("打开设置失败: {e}"))?;
        Ok(())
    }

    /// 在资源管理器中打开数据目录。用 `ShellExecuteW("open")` 而非 `explorer.exe <path>`：
    /// 后者在部分环境下静默失败/不聚焦，前者是打开文件夹的规范方式。先幂等建目录，
    /// 避免"尚未创建"挡住打开。
    pub fn open_userdata_dir(&self) -> Result<()> {
        let target = &self.cfg.userdata_dir;
        let _ = std::fs::create_dir_all(target);
        open_folder(target)
    }

    /// 在线更新：守卫标志 → 停服 → 解压覆盖当前目录 → 重启。返回是否需重启启动器自身
    /// （ZIP 替换了正在运行的 wind_portable.exe）。
    pub fn update_from_zip(&self, zip: &Path) -> Result<bool> {
        self.set_stopped_flag();
        let _ = self.stop_service();
        let needs_restart = match deploy::deploy_from_zip(zip, &self.cfg.root_dir) {
            Ok(v) => v,
            Err(e) => {
                self.clear_stopped_flag();
                bail!("文件替换失败: {e}");
            }
        };
        self.clear_stopped_flag();
        // 文件已替换：若重启失败，恢复 stopped 守卫使下次手动启动干净，并给出明确提示。
        if let Err(e) = self.start_service() {
            self.set_stopped_flag();
            bail!("文件已更新，但服务启动失败：{e}（请手动点击启动服务）");
        }
        Ok(needs_restart)
    }

    /// 复制部署：把当前便携包（除 userdata）复制到目标目录。
    pub fn deploy_to_directory(&self, target: &Path) -> Result<()> {
        deploy::deploy_from_directory(&self.cfg.root_dir, target)
    }

    /// 从 ZIP 部署到目标目录（不可为当前目录——原地更新请用 [`Self::update_from_zip`]，
    /// 它会先停服再替换，避免覆盖运行中的 exe/DLL）。
    pub fn deploy_zip_to_directory(&self, zip: &Path, target: &Path) -> Result<()> {
        if deploy::same_dir(&self.cfg.root_dir, target) {
            bail!("目标是当前目录；原地更新请改用『更新』按钮");
        }
        deploy::deploy_from_zip(zip, target).map(|_| ())
    }

    pub fn set_stopped_flag(&self) {
        self.write_marker(true);
    }

    pub fn clear_stopped_flag(&self) {
        self.write_marker(false);
    }

    // ── 内部 ──

    /// 冲突时报错（带动作名）。
    fn ensure_available(&self, action: &str) -> Result<()> {
        if let Some(reason) = self.installed_conflict(self.service_running()) {
            bail!("{action}失败：{reason}");
        }
        Ok(())
    }

    /// 关闭占据管道的他处残留实例：先试 RPC（预留），再按进程名兜底终止。
    fn shutdown_stale_service(&self) {
        if rpc::try_shutdown(&self.variant) {
            for _ in 0..6 {
                sleep(Duration::from_millis(500));
                if !self.service_running() {
                    return;
                }
            }
        }
        if let Some(stem) = self.service_stem() {
            process::terminate_by_name(&stem);
        }
    }

    /// 创建便携目录布局与标记文件。
    fn ensure_portable_layout(&self) -> Result<()> {
        let data = &self.cfg.userdata_dir;
        for sub in ["", "logs", "cache", "themes"] {
            let dir = if sub.is_empty() {
                data.clone()
            } else {
                data.join(sub)
            };
            std::fs::create_dir_all(&dir)
                .map_err(|e| anyhow!("创建目录失败 {}: {e}", dir.display()))?;
        }
        // 存量便携目录里只有旧名 `wind_portable_mode`，这里会补建新名完成迁移。
        // 旧名**刻意不删**：用户回退到旧版程序时它仍是唯一被认的标记，删了就退化成非便携。
        // 两名并存无害——读取侧一律新名优先（见 IPCClient.cpp 的 break）。
        if !self.cfg.portable_marker.is_file() {
            std::fs::write(&self.cfg.portable_marker, "wind_portable=1\n")
                .map_err(|e| anyhow!("写便携标记失败: {e}"))?;
        }
        Ok(())
    }

    /// 写便携标记（stopped=true 时附加守卫位）。
    fn write_marker(&self, stopped: bool) {
        let mut content = String::from("wind_portable=1\n");
        if stopped {
            content.push_str("stopped=1\n");
        }
        let _ = std::fs::write(&self.cfg.portable_marker, content);
    }

    /// 设置/移除开机自启（HKCU Run 键，值名 = 变体 app 名，与安装版一致）。
    fn set_autostart(&self, enable: bool) {
        if enable {
            let data = format!("\"{}\"", self.cfg.service_exe.display());
            let _ = reg::set_string(
                reg::HKEY_CURRENT_USER,
                RUN_KEY,
                self.variant.app_name,
                &data,
            );
        } else {
            let _ = reg::delete_value(reg::HKEY_CURRENT_USER, RUN_KEY, self.variant.app_name);
        }
    }
}

/// 用资源管理器打开文件夹（`ShellExecuteW("open")`）。返回值 >32 表示成功。
fn open_folder(path: &Path) -> Result<()> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let hinst = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if hinst.0 as isize > 32 {
        Ok(())
    } else {
        bail!("打开目录失败: {}", path.display())
    }
}
