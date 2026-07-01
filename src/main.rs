// GUI 子系统：不弹控制台窗口。CLI 动作时在 windows_main 内附着父进程控制台以输出。
#![cfg_attr(windows, windows_subsystem = "windows")]

mod cli;
mod deploy;
mod layout;
mod rpc;
mod variant;

#[cfg(windows)]
mod dialog;
#[cfg(windows)]
mod process;
#[cfg(windows)]
mod reg;
#[cfg(windows)]
mod registration;
#[cfg(windows)]
mod service;
#[cfg(windows)]
mod singleton;
#[cfg(windows)]
mod ui;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = cli::parse(&args);

    #[cfg(windows)]
    windows_main(opts);

    #[cfg(not(windows))]
    {
        let _ = opts;
        eprintln!("wind_portable 仅支持 Windows 平台。");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn windows_main(opts: cli::CliOptions) {
    let variant = variant::Variant::detect();
    let detected = layout::detect(&variant);

    // 旧文件清理（部署/更新替换残留的 *.old_*/*.bak）放后台，不阻塞启动。
    if let Ok(cfg) = &detected {
        let root = cfg.root_dir.clone();
        std::thread::spawn(move || deploy::clean_old_files(&root));
    }

    // CLI 动作（非 UI）：附着父控制台、直接执行、打印结果、退出。
    if opts.has_action() && !opts.ui {
        attach_parent_console();
        if let Err(e) = run_cli(&opts, &variant, detected) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }

    // GUI：单实例。已有实例运行 → 唤起其窗口后退出。
    let _guard = match singleton::acquire(&variant) {
        Some(g) => g,
        None => {
            singleton::activate_existing(&ui::launcher_title(&variant));
            return;
        }
    };

    // 探测失败也进入界面（显示错误原因）。
    let (manager, detect_error) = match detected {
        Ok(cfg) => (
            Some(service::ServiceManager::new(cfg, variant.clone())),
            None,
        ),
        Err(e) => (None, Some(e.to_string())),
    };
    ui::run(manager, &variant, detect_error);
}

/// 执行 CLI 动作。探测失败时对需要布局的动作返回错误。
#[cfg(windows)]
fn run_cli(
    opts: &cli::CliOptions,
    variant: &variant::Variant,
    detected: anyhow::Result<layout::PortableConfig>,
) -> anyhow::Result<()> {
    // 已提权的注册/注销分支（由 ShellExecuteEx runas 重新拉起执行）。
    if opts.elevate_register {
        return registration::register_direct(&detected?, variant);
    }
    if opts.elevate_unregister {
        registration::unregister_direct(&detected?, variant);
        return Ok(());
    }

    let mgr = service::ServiceManager::new(detected?, variant.clone());
    if opts.start {
        mgr.start_service()?;
        println!("service started");
    }
    if opts.stop {
        let stopped = mgr.stop_service()?;
        println!(
            "{}",
            if stopped {
                "service stopped"
            } else {
                "service not running"
            }
        );
    }
    if opts.status {
        let running = mgr.service_running();
        match mgr.installed_conflict(running) {
            Some(reason) => {
                println!(
                    "service={} mode=conflict reason=\"{reason}\"",
                    status_word(running)
                )
            }
            None => println!("service={}", status_word(running)),
        }
    }
    if opts.settings {
        mgr.open_settings()?;
        println!("settings opened");
    }
    if opts.userdata {
        mgr.open_userdata_dir()?;
        println!("userdata opened");
    }
    Ok(())
}

#[cfg(windows)]
fn status_word(running: bool) -> &'static str {
    if running {
        "running"
    } else {
        "stopped"
    }
}

/// 附着父进程控制台，使 GUI 子系统下的 CLI 输出可见于调用方终端（best-effort）。
#[cfg(windows)]
fn attach_parent_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
