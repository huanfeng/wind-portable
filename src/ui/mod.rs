//! 启动器 GUI（windui）：状态界面 + 托盘 + 部署/更新。Windows 专用。
//!
//! 并发模型（windui 0.4.0 重构）：
//! - **后台轮询线程**做全部阻塞 I/O（RPC 探活 + 注册表读取），结果经
//!   `App::channel` 的 `Sender<SnapMsg>` 发回 UI 线程；
//!   `on_message` 在 UI 线程写 `Signal<T>` 状态，框架自动产生局部脏区重绘。
//! - **UI 线程**只在 `on_message` 写 Signal，不做任何 I/O，不阻塞。
//! - 启停/部署/更新在后台线程跑 `Arc<ServiceManager>`；完成时发 `SnapMsg::Done`，
//!   最新快照与可选结果行一起写回 UI。

use std::sync::Arc;
use std::time::Duration;

use windui::prelude::*;

use crate::service::ServiceManager;
use crate::variant::Variant;
use crate::{deploy, dialog, layout};

const CARD: u32 = 0xFFFFFF;
const FG: u32 = 0x2D3436;
const SUB: u32 = 0x8A9099;
const ACCENT: u32 = 0x4C8BF5;

/// 窗口可见时后台轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(600);
/// 隐藏到托盘时的空闲间隔：不做 RPC/注册表轮询。
const IDLE_INTERVAL: Duration = Duration::from_millis(800);
/// 启动后确认"就绪"的最长等待。
const START_CONFIRM_TRIES: u32 = 40; // ×500ms ≈ 20s

pub fn launcher_title(variant: &Variant) -> String {
    if variant.is_dev {
        "清风输入法便携启动器 (Dev)".to_string()
    } else {
        "清风输入法便携启动器".to_string()
    }
}

/// 服务状态快照（后台线程算，经 channel 送 UI 线程）。
#[derive(Default, Clone, PartialEq)]
struct Snapshot {
    running: bool,
    registered: bool,
    conflict: Option<String>,
    conflict_path: Option<String>,
}

fn compute_snapshot(mgr: &ServiceManager) -> Snapshot {
    let running = mgr.service_running();
    let conflict = mgr.installed_conflict(running);
    let (registered, conflict_path) = if conflict.is_some() {
        (false, mgr.installed_conflict_path())
    } else {
        (mgr.is_registered(), None)
    };
    Snapshot {
        running,
        registered,
        conflict,
        conflict_path,
    }
}

/// 后台 → UI 线程消息。
enum SnapMsg {
    /// 周期轮询快照（busy 期间 UI 层忽略）。
    Poll(Snapshot),
    /// 动作完成：清 busy + 最新快照 + 可选结果行。
    Done {
        snap: Snapshot,
        outcome: Option<String>,
    },
}

/// 共享 UI 状态。
/// Signal<T> 是 Copy 索引句柄，Clone 几乎零开销（仅拷索引与 Arc 引用计数）。
#[derive(Clone)]
struct Ui {
    mgr: Option<Arc<ServiceManager>>,
    detect_error: Option<String>,
    title: String,
    root_dir: String,
    // 文本状态（Signal<String> 是 Copy，可被多个闭包同时"捕获"）
    status: Signal<String>,
    detail: Signal<String>,
    dir: Signal<String>,
    notice: Signal<String>,
    // 按钮启用状态
    en_start: Signal<bool>,
    en_stop: Signal<bool>,
    en_settings: Signal<bool>,
    en_data: Signal<bool>,
    en_update: Signal<bool>,
    en_deploy: Signal<bool>,
    // 后台操作进行中标志
    busy: Signal<bool>,
    // 后台线程发回 UI 线程的通道
    tx: Sender<SnapMsg>,
}

impl Ui {
    fn set_enables(
        &self,
        start: bool,
        stop: bool,
        settings: bool,
        data: bool,
        update: bool,
        deploy: bool,
    ) {
        self.en_start.set(start);
        self.en_stop.set(stop);
        self.en_settings.set(settings);
        self.en_data.set(data);
        self.en_update.set(update);
        self.en_deploy.set(deploy);
    }

    /// 把快照写入全部 Signal（在 UI 线程调用）。
    fn apply_snapshot(&self, snap: &Snapshot) {
        if let Some(de) = &self.detect_error {
            self.status.set("便携模式不可用".into());
            self.detail.set(de.clone());
            self.dir.set(String::new());
            self.set_enables(false, false, false, false, false, false);
            return;
        }
        if let Some(reason) = &snap.conflict {
            self.status.set("便携模式不可用".into());
            self.detail.set(reason.clone());
            self.dir.set(match &snap.conflict_path {
                Some(p) => format!("冲突位置: {p}"),
                None => format!("目录: {}", self.root_dir),
            });
            // 冲突时仍允许把当前文件部署到其他目录。
            self.set_enables(false, false, false, false, false, true);
            return;
        }
        let running = snap.running;
        let stoppable = running || snap.registered;
        self.status.set(
            if running {
                "服务状态: 运行中"
            } else {
                "服务状态: 已停止"
            }
            .into(),
        );
        self.detail.set({
            let n = self.notice.get();
            if !n.is_empty() {
                n
            } else if running {
                "输入法服务正在运行".into()
            } else {
                "点击启动服务后会自动注册并启动".into()
            }
        });
        self.dir.set(format!("目录: {}", self.root_dir));
        self.set_enables(!running, stoppable, running, true, true, true);
    }

    /// 进入"忙"态：设状态/详情/notice、禁用全部按钮、标记 busy。
    /// Signal::set() 在事件期内自动触发局部脏区，无需手写 ctx.mark_dirty()。
    fn begin_busy(&self, status: &str, detail: &str, notice: &str) {
        self.notice.set(notice.into());
        self.status.set(status.into());
        self.detail.set(detail.into());
        self.set_enables(false, false, false, false, false, false);
        self.busy.set(true);
    }

    fn owner(&self) -> Option<windows::Win32::Foundation::HWND> {
        dialog::find_main_window(&self.title)
    }

    /// 启动服务（后台线程，避免 UAC 等待冻结 UI；启动后轮询确认就绪）。
    fn start_clicked(&self) {
        let Some(mgr) = self.mgr.clone() else { return };
        self.begin_busy("正在启动服务...", "等待服务就绪", "");
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let r = mgr.start_service();
            if r.is_ok() {
                for _ in 0..START_CONFIRM_TRIES {
                    if mgr.service_running() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            let _ = tx.send(SnapMsg::Done {
                snap: compute_snapshot(&mgr),
                outcome: r.err().map(|e| e.to_string()),
            });
        });
    }

    /// 停止服务（后台线程）。
    fn stop_clicked(&self) {
        let Some(mgr) = self.mgr.clone() else { return };
        self.begin_busy("正在停止服务...", "", "");
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let r = mgr.stop_service();
            let _ = tx.send(SnapMsg::Done {
                snap: compute_snapshot(&mgr),
                outcome: r.err().map(|e| e.to_string()),
            });
        });
    }

    /// 打开设置（inline）。
    fn settings_clicked(&self) {
        if let Some(mgr) = &self.mgr {
            if let Err(e) = mgr.open_settings() {
                self.notice.set(e.to_string());
            }
        }
    }

    /// 打开数据目录（inline）。
    fn data_clicked(&self) {
        if let Some(mgr) = &self.mgr {
            if let Err(e) = mgr.open_userdata_dir() {
                self.notice.set(e.to_string());
            }
        }
    }

    /// 在线更新：选 ZIP → 校验 → 确认 → 后台停服/替换/重启。
    fn update_clicked(&self) {
        let Some(mgr) = self.mgr.clone() else { return };
        let owner = self.owner();
        let Some(zip) = dialog::pick_zip("选择便携版更新包") else {
            return;
        };
        if let Err(e) = deploy::validate_zip(&zip, &mgr.variant) {
            dialog::error(owner, &format!("无效的更新包：{e}"), &self.title);
            return;
        }
        if !dialog::confirm(
            owner,
            &format!("确认从以下文件更新便携版？\n\n{}", zip.display()),
            "确认更新",
        ) {
            return;
        }
        self.begin_busy(
            "正在更新...",
            "正在停止服务并替换文件",
            "正在更新：停止服务并替换文件…",
        );
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let outcome = match mgr.update_from_zip(&zip) {
                Ok(true) => Some("更新完成。启动器自身已更新，请关闭后重新打开。".to_string()),
                Ok(false) => Some("便携版更新完成。".to_string()),
                Err(e) => Some(e.to_string()),
            };
            let _ = tx.send(SnapMsg::Done {
                snap: compute_snapshot(&mgr),
                outcome,
            });
        });
    }

    /// 复制部署：选目标目录 → 确认 → 后台复制当前文件。
    fn deploy_copy_clicked(&self) {
        let Some(mgr) = self.mgr.clone() else { return };
        let owner = self.owner();
        let Some(target) = dialog::pick_folder("选择部署目标目录") else {
            return;
        };
        if layout::is_protected_dir(&target) {
            dialog::error(
                owner,
                &format!("不能部署到系统保护目录：\n{}", target.display()),
                &self.title,
            );
            return;
        }
        if !dialog::confirm(
            owner,
            &format!("确认将当前文件复制到：\n\n{}", target.display()),
            "确认部署",
        ) {
            return;
        }
        self.begin_busy(
            "正在部署...",
            "正在复制文件到目标目录",
            "正在复制文件到目标目录…",
        );
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let outcome = match mgr.deploy_to_directory(&target) {
                Ok(()) => Some(format!(
                    "已部署到：{}\n请到该目录运行 wind_portable.exe 启动。",
                    target.display()
                )),
                Err(e) => Some(format!("部署失败：{e}")),
            };
            let _ = tx.send(SnapMsg::Done {
                snap: compute_snapshot(&mgr),
                outcome,
            });
        });
    }

    /// 从 ZIP 部署到新目录：选 ZIP → 校验 → 选目标 → 确认 → 后台解压。
    fn deploy_zip_clicked(&self) {
        let Some(mgr) = self.mgr.clone() else { return };
        let owner = self.owner();
        let Some(zip) = dialog::pick_zip("选择便携版压缩包") else {
            return;
        };
        if let Err(e) = deploy::validate_zip(&zip, &mgr.variant) {
            dialog::error(owner, &format!("无效的压缩包：{e}"), &self.title);
            return;
        }
        let Some(target) = dialog::pick_folder("选择部署目标目录") else {
            return;
        };
        if layout::is_protected_dir(&target) {
            dialog::error(
                owner,
                &format!("不能部署到系统保护目录：\n{}", target.display()),
                &self.title,
            );
            return;
        }
        if !dialog::confirm(
            owner,
            &format!("确认将 ZIP 部署到：\n\n{}", target.display()),
            "确认部署",
        ) {
            return;
        }
        self.begin_busy(
            "正在部署...",
            "正在解压文件到目标目录",
            "正在解压文件到目标目录…",
        );
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let outcome = match mgr.deploy_zip_to_directory(&zip, &target) {
                Ok(()) => Some(format!(
                    "已部署到：{}\n请到该目录运行 wind_portable.exe 启动。",
                    target.display()
                )),
                Err(e) => Some(format!("部署失败：{e}")),
            };
            let _ = tx.send(SnapMsg::Done {
                snap: compute_snapshot(&mgr),
                outcome,
            });
        });
    }
}

/// 后台轮询线程：窗口可见时周期算快照、变化时经 channel 推给 UI 线程；
/// 隐藏到托盘时**暂停**轮询，仅低频探测可见性，并在刚隐藏那刻回收工作集。
fn spawn_bg_poller(mgr: Arc<ServiceManager>, tx: Sender<SnapMsg>, title: String) {
    std::thread::spawn(move || {
        let mut prev: Option<Snapshot> = None;
        let mut was_visible = true;
        loop {
            let visible = window_visible(&title);
            if visible {
                was_visible = true;
                let snap = compute_snapshot(&mgr);
                let changed = prev.as_ref() != Some(&snap);
                prev = Some(snap.clone());
                if changed {
                    let _ = tx.send(SnapMsg::Poll(snap));
                }
                std::thread::sleep(POLL_INTERVAL);
            } else {
                if was_visible {
                    trim_working_set();
                    was_visible = false;
                    prev = None; // 再次显示时强制推送快照
                }
                std::thread::sleep(IDLE_INTERVAL);
            }
        }
    });
}

/// 回收当前进程工作集（隐藏到托盘时降低任务管理器显示的内存占用）。
fn trim_working_set() {
    use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
    use windows::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

fn window_visible(title: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;
    dialog::find_main_window(title)
        .map(|h| unsafe { IsWindowVisible(h).as_bool() })
        .unwrap_or(false)
}

/// 内嵌的启动器图标。
const APP_ICO: &[u8] = include_bytes!("../../res/app.ico");

fn solid_icon(hex: u32) -> Vec<u8> {
    let (r, g, b) = (
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    );
    [r, g, b, 255].repeat(16 * 16)
}

/// 解码内嵌 app.ico，取最接近 `size` 的一档为 RGBA8。失败回退纯色。
fn app_icon_rgba(size: u32) -> (u32, u32, Vec<u8>) {
    if let Ok(dir) = ico::IconDir::read(std::io::Cursor::new(APP_ICO)) {
        let entry = dir
            .entries()
            .iter()
            .find(|e| e.width() == size)
            .or_else(|| {
                dir.entries()
                    .iter()
                    .filter(|e| e.width() >= size)
                    .min_by_key(|e| e.width())
            })
            .or_else(|| dir.entries().iter().max_by_key(|e| e.width()));
        if let Some(entry) = entry {
            if let Ok(img) = entry.decode() {
                return (img.width(), img.height(), img.rgba_data().to_vec());
            }
        }
    }
    (16, 16, solid_icon(ACCENT))
}

/// 运行 GUI。`manager` 为 None 表示便携布局探测失败（`detect_error` 含原因）。
pub fn run(manager: Option<ServiceManager>, variant: &Variant, detect_error: Option<String>) {
    let mgr = manager.map(Arc::new);
    let title = launcher_title(variant);
    let root_dir = mgr
        .as_ref()
        .map(|m| m.cfg.root_dir.display().to_string())
        .unwrap_or_default();

    // 所有 Signal 在建 App 前分配（线程局部 arena）。
    // Signal<T>: Copy，同一 slot 索引可被多个闭包独立捕获。
    let status: Signal<String> = signal("正在检查服务状态...".into());
    let detail: Signal<String> = signal(String::new());
    let dir: Signal<String> = signal(String::new());
    let notice: Signal<String> = signal(String::new());
    let en_start: Signal<bool> = signal(false);
    let en_stop: Signal<bool> = signal(false);
    let en_settings: Signal<bool> = signal(false);
    let en_data: Signal<bool> = signal(false);
    let en_update: Signal<bool> = signal(false);
    let en_deploy: Signal<bool> = signal(false);
    let busy: Signal<bool> = signal(false);

    // resizable(false)：去掉 WS_THICKFRAME 与最大化按钮，窗口尺寸固定。
    // 尺寸固定后布局无法手动救回，故窗口略留余量、由 run_page 末尾的弹性占位吸收。
    let mut app = App::new(title.clone(), 430, 262)
        .bg(Color::hex(CARD))
        .resizable(false);

    // 跨线程通道：后台 send → UI 线程 on_message 写 Signal → 框架局部重绘。
    // Signal 是 Copy，闭包直接捕获（无需 clone）。
    let de = detect_error.clone();
    let rd = root_dir.clone();
    let tx = app.channel::<SnapMsg>(move |msg| {
        let snap = match msg {
            SnapMsg::Poll(snap) => {
                if busy.get() {
                    return;
                } // busy 期间忽略轮询快照
                snap
            }
            SnapMsg::Done { snap, outcome } => {
                busy.set(false);
                if let Some(o) = outcome {
                    notice.set(o);
                }
                snap
            }
        };
        apply_snapshot_to_signals(
            &snap,
            de.as_deref(),
            &rd,
            status,
            detail,
            dir,
            notice,
            en_start,
            en_stop,
            en_settings,
            en_data,
            en_update,
            en_deploy,
        );
    });

    // Ui 持有相同的 Signal 索引（Copy 复制，与 channel 闭包共享同一 slot）。
    let ui = Ui {
        mgr: mgr.clone(),
        detect_error: detect_error.clone(),
        title: title.clone(),
        root_dir: root_dir.clone(),
        status,
        detail,
        dir,
        notice,
        en_start,
        en_stop,
        en_settings,
        en_data,
        en_update,
        en_deploy,
        busy,
        tx: tx.clone(),
    };

    // 初始快照：UI 线程直接写 Signal（无需走 channel）。
    if let Some(m) = &mgr {
        ui.apply_snapshot(&compute_snapshot(m));
    } else if let Some(de) = &detect_error {
        status.set("便携模式不可用".into());
        detail.set(de.clone());
    }

    // 后台轮询线程（窗口可见时每 600ms 推送一次快照）。
    if let Some(m) = mgr {
        spawn_bg_poller(m, tx, title.clone());
    }

    let tray = build_tray(&ui);
    let content = build_content(&ui);
    app.tray(tray).content(content).run();
}

/// 把快照写入一组 Signal（供 channel on_message 和初始化共用的无 self 版本）。
#[allow(clippy::too_many_arguments)]
fn apply_snapshot_to_signals(
    snap: &Snapshot,
    detect_error: Option<&str>,
    root_dir: &str,
    status: Signal<String>,
    detail: Signal<String>,
    dir: Signal<String>,
    notice: Signal<String>,
    en_start: Signal<bool>,
    en_stop: Signal<bool>,
    en_settings: Signal<bool>,
    en_data: Signal<bool>,
    en_update: Signal<bool>,
    en_deploy: Signal<bool>,
) {
    if let Some(de) = detect_error {
        status.set("便携模式不可用".into());
        detail.set(de.into());
        dir.set(String::new());
        en_start.set(false);
        en_stop.set(false);
        en_settings.set(false);
        en_data.set(false);
        en_update.set(false);
        en_deploy.set(false);
        return;
    }
    if let Some(reason) = &snap.conflict {
        status.set("便携模式不可用".into());
        detail.set(reason.clone());
        dir.set(match &snap.conflict_path {
            Some(p) => format!("冲突位置: {p}"),
            None => format!("目录: {root_dir}"),
        });
        en_start.set(false);
        en_stop.set(false);
        en_settings.set(false);
        en_data.set(false);
        en_update.set(false);
        en_deploy.set(true);
        return;
    }
    let running = snap.running;
    let stoppable = running || snap.registered;
    status.set(
        if running {
            "服务状态: 运行中"
        } else {
            "服务状态: 已停止"
        }
        .into(),
    );
    detail.set({
        let n = notice.get();
        if !n.is_empty() {
            n
        } else if running {
            "输入法服务正在运行".into()
        } else {
            "点击启动服务后会自动注册并启动".into()
        }
    });
    dir.set(format!("目录: {root_dir}"));
    en_start.set(!running);
    en_stop.set(stoppable);
    en_settings.set(running);
    en_data.set(true);
    en_update.set(true);
    en_deploy.set(true);
}

/// 构建托盘（图标 + 菜单）。
/// Signal<bool> 是 Copy，直接传给 `.enabled()` 和在回调中读取，无需 clone。
fn build_tray(ui: &Ui) -> Tray {
    let (u_start, u_stop, u_set, u_data) = (ui.clone(), ui.clone(), ui.clone(), ui.clone());
    let (icw, ich, icon) = app_icon_rgba(32);
    let toggle_title = ui.title.clone();

    Tray::new()
        .tooltip(ui.title.as_str())
        .icon_rgba(icw, ich, &icon)
        // 单击：切换窗口显隐。
        .on_left_click(move |ctx| {
            if window_visible(&toggle_title) {
                ctx.hide_window();
            } else {
                ctx.show_window();
            }
        })
        .on_double_click(|ctx| ctx.show_window())
        .menu(vec![
            TrayMenuItem::item("显示窗口", |ctx| ctx.show_window()),
            TrayMenuItem::item("隐藏到托盘", |ctx| ctx.hide_window()),
            TrayMenuItem::separator(),
            TrayMenuItem::item("启动服务", move |ctx| {
                if u_start.en_start.get() {
                    u_start.start_clicked();
                } else {
                    ctx.notify(&u_start.title, "服务已在运行");
                }
            })
            .enabled(ui.en_start),
            TrayMenuItem::item("停止服务", move |ctx| {
                if u_stop.en_stop.get() {
                    u_stop.stop_clicked();
                } else {
                    ctx.notify(&u_stop.title, "服务未在运行");
                }
            })
            .enabled(ui.en_stop),
            TrayMenuItem::item("打开设置", move |ctx| {
                if u_set.en_settings.get() {
                    u_set.settings_clicked();
                } else {
                    ctx.notify(&u_set.title, "请先启动服务再打开设置");
                }
            })
            .enabled(ui.en_settings),
            TrayMenuItem::item("数据目录", move |_| u_data.data_clicked()).enabled(ui.en_data),
            TrayMenuItem::separator(),
            TrayMenuItem::item("退出", |ctx| ctx.quit()),
        ])
}

/// 构建窗口控件树：TAB（运行 / 部署）。
/// 去掉了旧版"隐藏 visible_when 同步器"——Signal::set() 自动触发脏区，无需该 hack。
fn build_content(ui: &Ui) -> Element {
    // Signal<bool> 是 Copy，action_button 直接持有，无需 Rc 包装。
    let action_button = |label: &str, en: Signal<bool>, on: Box<dyn Fn()>| {
        Element::button(label)
            .enabled(en)
            .weight(1.0)
            .height(30)
            .on_click(move |_| on())
    };
    let row = || Element::row().width_match().spacing(10);

    let (u_start, u_stop, u_set, u_data) = (ui.clone(), ui.clone(), ui.clone(), ui.clone());
    let (u_update, u_dcopy, u_dzip) = (ui.clone(), ui.clone(), ui.clone());

    // 「运行」页：状态/详情/目录 + 启停/设置/数据（对应原 C# tabRun）。
    let run_page = Element::col()
        .fill()
        .padding(10)
        .spacing(6)
        .child(
            // label_rc 绑定 Signal<String>，Signal 变化时自动局部刷新文字。
            Element::label_rc(ui.status)
                .font_size(16.0)
                .fg(Color::hex(FG))
                .height(22)
                .width_match(),
        )
        .child(
            Element::label_rc(ui.detail)
                .font_size(13.0)
                .fg(Color::hex(SUB))
                .height(40)
                .width_match(),
        )
        .child(
            Element::label_rc(ui.dir)
                .font_size(12.0)
                .fg(Color::hex(SUB))
                .height(22)
                .width_match(),
        )
        .child(
            row()
                .child(action_button(
                    "启动服务",
                    ui.en_start,
                    Box::new(move || u_start.start_clicked()),
                ))
                .child(action_button(
                    "停止服务",
                    ui.en_stop,
                    Box::new(move || u_stop.stop_clicked()),
                )),
        )
        .child(
            row()
                .child(action_button(
                    "打开设置",
                    ui.en_settings,
                    Box::new(move || u_set.settings_clicked()),
                ))
                .child(action_button(
                    "打开数据目录",
                    ui.en_data,
                    Box::new(move || u_data.data_clicked()),
                )),
        )
        .child(Element::label("").weight(1.0).width_match());

    // 「部署」页：提示 + 更新/复制/ZIP + 结果行（对应原 C# tabDeploy）。
    let deploy_page = Element::col()
        .fill()
        .padding(10)
        .spacing(6)
        .child(
            Element::label("更新当前安装、复制当前文件到新目录、或从 ZIP 包部署到新目录。")
                .font_size(12.0)
                .fg(Color::hex(SUB))
                .height(36)
                .width_match(),
        )
        .child(
            row()
                .child(action_button(
                    "更新当前版本",
                    ui.en_update,
                    Box::new(move || u_update.update_clicked()),
                ))
                .child(action_button(
                    "复制到目录",
                    ui.en_deploy,
                    Box::new(move || u_dcopy.deploy_copy_clicked()),
                ))
                .child(action_button(
                    "从 ZIP 部署",
                    ui.en_deploy,
                    Box::new(move || u_dzip.deploy_zip_clicked()),
                )),
        )
        .child(
            // 部署页结果/进度行：只绑 notice，空闲为空白。
            Element::label_rc(ui.notice)
                .font_size(12.0)
                .fg(Color::hex(SUB))
                .width_match()
                .weight(1.0),
        );

    // Signal<usize> 替代旧版 Rc<Cell<usize>>。
    let selected_tab = signal(0usize);
    let tabs = Element::tabs(
        selected_tab,
        vec![("运行", run_page), ("部署", deploy_page)],
    )
    .weight(1.0);

    Element::col()
        .fill()
        .bg(Color::hex(CARD))
        .padding(4)
        .child(tabs)
}
