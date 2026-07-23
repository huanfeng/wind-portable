//! 启动器 GUI（windui）：状态界面 + 托盘 + 部署/更新。Windows 专用。
//!
//! 并发模型（windui 0.4.0 重构）：
//! - **后台轮询线程**做全部阻塞 I/O（RPC 探活 + 注册表读取），结果经
//!   `App::channel` 的 `Sender<SnapMsg>` 发回 UI 线程；
//!   `on_message` 在 UI 线程写 `Signal<T>` 状态，框架自动产生局部脏区重绘。
//! - **UI 线程**只在 `on_message` 写 Signal，不做任何 I/O，不阻塞。
//! - 启停/部署/更新在后台线程跑 `Arc<ServiceManager>`；完成时发 `SnapMsg::Done`，
//!   最新快照与可选结果行一起写回 UI。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windui::core::EventCtx;
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
/// 托盘菜单启用态同步间隔。仅在值变化时写 Signal，空闲不产生重绘；
/// 决定隐藏态下托盘菜单反映状态变化的最大额外滞后。
const TRAY_ENABLE_SYNC: Duration = Duration::from_millis(400);


/// 退出流程的跨线程状态。用 Atomic 而非 Signal：确认流程整个跑在后台线程，
/// 而 Signal 存活于 UI 线程的线程局部 arena，只能在 UI 线程读写。
#[derive(Default)]
struct ExitState {
    /// 退出已获准：置位后拦截器直接放行。
    allowed: AtomicBool,
    /// 确认流程进行中：拦截住重复的关闭请求（标题栏 X 与托盘「退出」都会投递
    /// `WM_CLOSE`，而拦截器返回 false 只取消当次关闭、不阻止用户再点一次）。
    /// 缺了这道闸门，确认框会一个叠一个地弹出来。
    asking: AtomicBool,
    /// 本次 `WM_CLOSE` 来自托盘「退出」而非标题栏 ×：区分"真要退出"与"收进托盘"。
    /// 两者是同一条消息，不带标记就无从分辨。
    intent: AtomicBool,
}

impl ExitState {
    /// 抢占确认流程的所有权。已有确认在进行时返回 false（调用方应直接放弃）。
    fn begin_ask(&self) -> bool {
        self.asking
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// 确认流程收尾。一并清 `intent`：用户选了「取消」后这次退出意图就作废了，
    /// 留着会让下一次点 × 误走退出流程而不是收进托盘。
    fn end_ask(&self) {
        self.intent.store(false, Ordering::SeqCst);
        self.asking.store(false, Ordering::SeqCst);
    }

    /// 放行退出：置标志后重新投递 `WM_CLOSE`，让拦截器第二次进来时直接通过。
    /// 走 `WM_CLOSE` 而非 `ExitProcess`，是为了让 windui 正常走完 `WM_DESTROY`
    /// —— 托盘图标在那里 `NIM_DELETE`，硬退会在通知区留下要鼠标划过才消失的僵尸图标。
    fn allow(&self, owner: Option<windows::Win32::Foundation::HWND>) {
        self.allowed.store(true, Ordering::SeqCst);
        post_close(owner);
    }
}

/// 仅在值变化时写 Signal。`Signal::set` 每次都标脏请求重绘（不去重），
/// 周期性同步必须先比较，否则可见态每个定时器拍都请求重绘、空闲 CPU 回不到 0。
fn set_if(sig: Signal<bool>, v: bool) {
    if sig.get() != v {
        sig.set(v);
    }
}

/// 隐藏主窗口（关闭按钮 → 收进托盘）。`ShowWindow` 可跨线程调用。
fn hide_main_window(hwnd: Option<windows::Win32::Foundation::HWND>) {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    if let Some(h) = hwnd {
        unsafe {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
}

/// 向主窗口投递 `WM_CLOSE`（跨线程安全：`PostMessageW` 只入队，不等待处理）。
fn post_close(hwnd: Option<windows::Win32::Foundation::HWND>) {
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    if let Some(h) = hwnd {
        unsafe {
            let _ = PostMessageW(Some(h), WM_CLOSE, Default::default(), Default::default());
        }
    }
}

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
    // 后台操作进行中标志（仅供 UI 呈现：忽略轮询快照、置灰按钮）。
    busy: Signal<bool>,
    // 同一事实的权威副本，供**非 UI 线程或窗口隐藏时**判断。
    //
    // 不能拿 `busy` Signal 当真相：它由后台线程经 channel 送回、在 `on_message` 里写，
    // 而 windui 排空 channel 的 `pump()` 只跑在 `render()` 里（app.rs:1162）——
    // Windows 不给隐藏窗口发 WM_PAINT，`Sender::send` 的 `InvalidateRect` 唤不出帧，
    // 于是窗口收进托盘期间做的动作，其 `Done` 会一直躺在队列里，`busy` 永远停在 true
    // （直到窗口再次显示才补上）。曾导致"托盘停服后无法退出，一直提示正在部署/更新"。
    working: Arc<AtomicBool>,
    // 最新快照的权威副本，理由同 `working`：Signal 要等绘制帧才更新，而窗口收进
    // 托盘后再无绘制帧，此时托盘菜单是唯一的界面，只能读这里。
    snap: Arc<Mutex<Snapshot>>,
    // 退出流程状态（见 ExitState）。
    exit: Arc<ExitState>,
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
        self.working.store(true, Ordering::SeqCst);
    }

    /// 后台动作收尾：清权威标志，再把最新快照与结果送回 UI 线程。
    ///
    /// 顺序要紧：`working` 必须先于 `send` 清掉。`send` 只是入队 + 请求一帧，窗口隐藏
    /// 时那一帧不会到来（见 [`Ui::working`]），此时唯一还准的就是这个标志。
    fn finish_action(&self, mgr: &ServiceManager, outcome: Option<String>) {
        let snap = compute_snapshot(mgr);
        self.publish(&snap);
        self.working.store(false, Ordering::SeqCst);
        let _ = self.tx.send(SnapMsg::Done { snap, outcome });
    }

    /// 更新权威快照副本（任意线程可调）。
    fn publish(&self, snap: &Snapshot) {
        if let Ok(mut g) = self.snap.lock() {
            *g = snap.clone();
        }
    }

    /// 读权威快照副本。
    fn snapshot(&self) -> Snapshot {
        self.snap.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// 六个按钮的期望启用态 `(start, stop, settings, data, update, deploy)`。
    ///
    /// 唯一真值来源：托盘 `.enabled` 同步、托盘点击校验、窗口按钮都据此，逻辑不再各写一份。
    /// 四种情形分层：探测失败 → 冲突 → 正忙 → 正常，与 `apply_snapshot_to_signals` 对齐。
    fn desired_enables(&self) -> (bool, bool, bool, bool, bool, bool) {
        if self.detect_error.is_some() {
            return (false, false, false, false, false, false);
        }
        let s = self.snapshot();
        if s.conflict.is_some() {
            // 冲突态只放行"复制/ZIP 部署到其他目录"。
            return (false, false, false, false, false, true);
        }
        if self.working.load(Ordering::SeqCst) {
            return (false, false, false, false, false, false);
        }
        let running = s.running;
        let stoppable = running || s.registered;
        (!running, stoppable, running, true, true, true)
    }

    /// 把期望启用态同步进 en_* Signal（**必须在 UI 线程调**，如 `on_interval`）。
    ///
    /// 托盘菜单 `.enabled` 在右键弹出时实时读 Signal，而 channel 只在绘制帧刷新 Signal——
    /// 窗口收进托盘后没有绘制帧，Signal 会停在收起那刻，菜单灰显随之卡死。靠 WM_TIMER
    /// （隐藏态照常触发）周期重算写入，弹出的菜单才拿得到当前状态。
    /// **仅在值变化时 set**：`Signal::set` 不去重，无条件写会让可见态每拍请求重绘（毁掉空闲 0 CPU）。
    fn refresh_enables(&self) {
        let (start, stop, settings, data, update, deploy) = self.desired_enables();
        set_if(self.en_start, start);
        set_if(self.en_stop, stop);
        set_if(self.en_settings, settings);
        set_if(self.en_data, data);
        set_if(self.en_update, update);
        set_if(self.en_deploy, deploy);
    }

    /// 托盘「启动服务」此刻是否可用。
    fn can_start(&self) -> bool {
        self.desired_enables().0
    }

    /// 托盘「停止服务」此刻是否可用。注册残留（服务已挂但 TSF 还在）也算可停。
    fn can_stop(&self) -> bool {
        self.desired_enables().1
    }

    /// 动作不可用时的气泡说明。冲突与"正忙"要说清，否则用户只看到点了没反应。
    fn blocked_reason(&self, default: &str) -> String {
        if self.working.load(Ordering::SeqCst) {
            return "正在执行上一个操作，请稍候".into();
        }
        match self.snapshot().conflict {
            Some(reason) => reason,
            None => default.into(),
        }
    }

    fn owner(&self) -> Option<windows::Win32::Foundation::HWND> {
        dialog::find_main_window(&self.title)
    }

    /// 启动服务（后台线程，避免 UAC 等待冻结 UI；启动后轮询确认就绪）。
    fn start_clicked(&self) {
        let Some(mgr) = self.mgr.clone() else { return };
        self.begin_busy("正在启动服务...", "等待服务就绪", "");
        let this = self.clone();
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
            this.finish_action(&mgr, r.err().map(|e| e.to_string()));
        });
    }

    /// 停止服务（后台线程）。
    fn stop_clicked(&self) {
        let Some(mgr) = self.mgr.clone() else { return };
        self.begin_busy("正在停止服务...", "", "");
        let this = self.clone();
        std::thread::spawn(move || {
            let r = mgr.stop_service();
            this.finish_action(&mgr, r.err().map(|e| e.to_string()));
        });
    }

    /// 发起退出（托盘「退出」菜单）。
    ///
    /// **不能用 `ctx.quit()`**：windui 的 `TrayCtx::quit()` 直接 `DestroyWindow`，
    /// 绕过 `WM_CLOSE`，`on_close_request` 拦截器根本不会被调用——托盘退出就会跳过
    /// 卸载确认。改为标记"这是真退出"后投递 `WM_CLOSE`，汇入同一条拦截链。
    fn request_exit(&self) {
        self.exit.intent.store(true, Ordering::SeqCst);
        post_close(self.owner());
    }

    /// 标题栏 × / ESC 的关闭请求：服务在跑时**收进托盘而非退出**。
    ///
    /// 输入法启动器的「关闭」几乎总是"我看完了，收起来"，而不是"把输入法卸了"——
    /// 后者代价太大且不可逆（要重新注册 TSF）。真正的退出留给托盘右键「退出」。
    /// 服务没在跑时窗口已无常驻价值，直接走退出流程（无残留则静默退出）。
    fn close_clicked(&self) -> bool {
        // 确认流程放行后重投的 WM_CLOSE：直接过。
        // **必须排在收纳分支之前**——选了「否」（保留服务）时服务仍在跑，
        // 落到下面就会被收进托盘，退不掉。
        if self.exit.allowed.load(Ordering::SeqCst) {
            return true;
        }
        // 托盘「退出」投递来的 WM_CLOSE：直奔退出流程，不做收纳。
        if self.exit.intent.load(Ordering::SeqCst) {
            return self.confirm_exit();
        }
        // 冲突态（已安装版本占用）→ 直接退出，不收托盘。
        // 此时探活探到的 running 是**安装版**的服务，不是我们的；启动器本身被禁用、
        // 无任何驻留价值，收进托盘只会让用户以为"关不掉"。confirm_exit 也会因冲突
        // 走直接放行（无残留可清理），这里提前拦下省掉一次无谓的 WM_CLOSE 往返。
        let snap = self.snapshot();
        if snap.conflict.is_some() {
            return true;
        }
        // 服务在跑 → 收进托盘。返回 false 取消关闭，窗口由 hide_main_window 隐藏。
        // 探测失败（无 mgr）时没有服务可守候，不留托盘。
        //
        // 读缓存快照而非现探活：本回调运行在 WndProc 持有窗口状态借用的临界区里，
        // 不宜做阻塞 I/O。这里只决定"收起还是退出"，最多差一个轮询周期；真正有
        // 后果的卸载判断在 confirm_exit 的后台线程里重新探活。
        if self.mgr.is_some() && snap.running {
            hide_main_window(self.owner());
            return false;
        }
        self.confirm_exit()
    }

    /// 关闭请求拦截器（`App::on_close_request`）：服务仍在运行或注册未撤销时，
    /// 先问用户是否顺带卸载，再决定是否放行。返回 `true` 放行、`false` 取消本次关闭。
    ///
    /// **回调内绝不能同步弹模态框**：它由 WndProc 在持有窗口状态可变借用的临界区里
    /// 调用，而 `MessageBoxW` 自带嵌套消息泵——泵到 `WM_PAINT` 就会重入 WndProc 再取
    /// 一次同一份状态（裸指针别名 UB）。因此这里只做 UI 线程上非阻塞的 Signal 读取，
    /// 弹框、探活与停服全部丢给后台线程，本次调用立即返回 `false`；线程拿到结果后置
    /// `exiting` 并重新投递 `WM_CLOSE`，第二次进来在开头直接放行。
    fn confirm_exit(&self) -> bool {
        if self.exit.allowed.load(Ordering::SeqCst) {
            return true;
        }
        // 已有确认框在弹了：忽略这次请求，别再叠一个。
        if !self.exit.begin_ask() {
            return false;
        }
        // 读权威标志而非 `busy` Signal：窗口收在托盘里时 Signal 是陈旧的（见 Ui::working）。
        let busy = self.working.load(Ordering::SeqCst);
        // 冲突态（已安装版本占用）：启动器未注册未起进程，无副作用可清理，直接放行。
        let conflict = self.snapshot().conflict.is_some();
        // HWND 非 Send，拆成裸整数过线程边界，用时再还原。
        let owner_raw = self.owner().map(|h| h.0 as isize);
        let (title, mgr, exit) = (self.title.clone(), self.mgr.clone(), self.exit.clone());

        std::thread::spawn(move || {
            let owner = owner_raw.map(|v| windows::Win32::Foundation::HWND(v as *mut _));
            // 部署/更新进行中：此刻退出会留下半替换的文件树，先拦住。
            if busy {
                dialog::error(owner, "正在部署/更新，请等待完成后再退出。", &title);
            } else if conflict {
                exit.allow(owner);
            } else if let Some(mgr) = mgr {
                // 服务未跑且注册已撤销 → 无残留，不打扰用户。
                if !mgr.service_running() && !mgr.is_registered() {
                    exit.allow(owner);
                } else {
                    let answer = dialog::ask_commands(
                        owner,
                        &title,
                        "清风输入法仍处于已启用状态",
                        ("停止服务并卸载", "移除开机自启与 TSF 注册"),
                        ("仅关闭启动器", "输入法继续可用，稍后可再启动"),
                    );
                    match answer {
                        // 卸载：停服 + 移除自启 + 注销 TSF（可能触发 UAC，故必须在后台线程）。
                        dialog::Answer::Yes => match mgr.stop_service() {
                            Ok(_) => exit.allow(owner),
                            // 残留没清干净就别退出，留在原地让用户重试或改选「否」。
                            Err(e) => dialog::error(owner, &format!("卸载失败：{e}"), &title),
                        },
                        // 保留：仅关启动器，输入法继续可用（符合输入法直觉）。
                        dialog::Answer::No => exit.allow(owner),
                        dialog::Answer::Cancel => {}
                    }
                }
            } else {
                // 布局探测失败（无 mgr）：没有副作用可清理，直接放行。
                exit.allow(owner);
            }
            exit.end_ask();
        });
        false
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
    ///
    /// 整段"选文件→校验→确认"都是阻塞式原生模态调用（`PickDialog`/`MessageBoxW`），
    /// 必须经 `ctx.defer_blocking` 延后到事件分发完全返回之后才执行——直接在
    /// `on_click` 回调里同步调用会在 OS 鼠标捕获尚未释放时重入模态对话框的消息泵，
    /// 反复点击/关闭几次就会把鼠标彻底锁死（同一类问题见 wind-ui-rust 的
    /// `DialogRequest` 文档）。闭包内可以放心连续弹多个原生模态框：此时已经不在
    /// 事件回调栈内了。
    fn update_clicked(&self, ctx: &mut EventCtx) {
        let this = self.clone();
        ctx.defer_blocking(move || {
            let Some(mgr) = this.mgr.clone() else { return };
            let owner = this.owner();
            let Some(zip) = dialog::pick_zip("选择便携版更新包") else {
                return;
            };
            if let Err(e) = deploy::validate_zip(&zip, &mgr.variant) {
                dialog::error(owner, &format!("无效的更新包：{e}"), &this.title);
                return;
            }
            if !dialog::confirm(
                owner,
                &format!("确认从以下文件更新便携版？\n\n{}", zip.display()),
                "确认更新",
            ) {
                return;
            }
            this.begin_busy(
                "正在更新...",
                "正在停止服务并替换文件",
                "正在更新：停止服务并替换文件…",
            );
            std::thread::spawn(move || {
                let outcome = match mgr.update_from_zip(&zip) {
                    Ok(true) => Some("更新完成。启动器自身已更新，请关闭后重新打开。".to_string()),
                    Ok(false) => Some("便携版更新完成。".to_string()),
                    Err(e) => Some(e.to_string()),
                };
                this.finish_action(&mgr, outcome);
            });
        });
    }

    /// 复制部署：选目标目录 → 确认 → 后台复制当前文件。
    /// 阻塞式原生调用延后到事件分发返回后执行，理由同 [`Ui::update_clicked`]。
    fn deploy_copy_clicked(&self, ctx: &mut EventCtx) {
        let this = self.clone();
        ctx.defer_blocking(move || {
            let Some(mgr) = this.mgr.clone() else { return };
            let owner = this.owner();
            let Some(target) = dialog::pick_folder("选择部署目标目录") else {
                return;
            };
            if layout::is_protected_dir(&target) {
                dialog::error(
                    owner,
                    &format!("不能部署到系统保护目录：\n{}", target.display()),
                    &this.title,
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
            this.begin_busy(
                "正在部署...",
                "正在复制文件到目标目录",
                "正在复制文件到目标目录…",
            );
            std::thread::spawn(move || {
                let outcome = match mgr.deploy_to_directory(&target) {
                    Ok(()) => Some(format!(
                        "已部署到：{}\n请到该目录运行 wind_portable.exe 启动。",
                        target.display()
                    )),
                    Err(e) => Some(format!("部署失败：{e}")),
                };
                this.finish_action(&mgr, outcome);
            });
        });
    }

    /// 从 ZIP 部署到新目录：选 ZIP → 校验 → 选目标 → 确认 → 后台解压。
    /// 阻塞式原生调用延后到事件分发返回后执行，理由同 [`Ui::update_clicked`]。
    fn deploy_zip_clicked(&self, ctx: &mut EventCtx) {
        let this = self.clone();
        ctx.defer_blocking(move || {
            let Some(mgr) = this.mgr.clone() else { return };
            let owner = this.owner();
            let Some(zip) = dialog::pick_zip("选择便携版压缩包") else {
                return;
            };
            if let Err(e) = deploy::validate_zip(&zip, &mgr.variant) {
                dialog::error(owner, &format!("无效的压缩包：{e}"), &this.title);
                return;
            }
            let Some(target) = dialog::pick_folder("选择部署目标目录") else {
                return;
            };
            if layout::is_protected_dir(&target) {
                dialog::error(
                    owner,
                    &format!("不能部署到系统保护目录：\n{}", target.display()),
                    &this.title,
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
            this.begin_busy(
                "正在部署...",
                "正在解压文件到目标目录",
                "正在解压文件到目标目录…",
            );
            std::thread::spawn(move || {
                let outcome = match mgr.deploy_zip_to_directory(&zip, &target) {
                    Ok(()) => Some(format!(
                        "已部署到：{}\n请到该目录运行 wind_portable.exe 启动。",
                        target.display()
                    )),
                    Err(e) => Some(format!("部署失败：{e}")),
                };
                this.finish_action(&mgr, outcome);
            });
        });
    }
}

/// 后台轮询线程：窗口可见时周期算快照、变化时经 channel 推给 UI 线程；
/// 隐藏到托盘时**暂停**轮询，仅低频探测可见性，并在刚隐藏那刻回收工作集。
fn spawn_bg_poller(ui: Ui, mgr: Arc<ServiceManager>, title: String) {
    std::thread::spawn(move || {
        let mut prev: Option<Snapshot> = None;
        let mut was_visible = true;
        loop {
            let visible = window_visible(&title);
            // 隐藏时也照常算快照，只是放慢节奏：托盘菜单读的是权威副本，
            // 而它此刻是唯一的界面。此前隐藏即完全停摆，收进托盘后停服，
            // 菜单项会一直卡在旧状态（「启动服务」灰着点不动）。
            // 动作执行期间跳过：finish_action 会给出更权威的收尾快照，
            // 轮询插进来只会用中间态覆盖它。
            if !ui.working.load(Ordering::SeqCst) {
                let snap = compute_snapshot(&mgr);
                ui.publish(&snap);
                let changed = prev.as_ref() != Some(&snap);
                prev = Some(snap.clone());
                // 仅在窗口可见时推 UI：隐藏时没有绘制帧，消息只会堆在队列里。
                if changed && visible {
                    let _ = ui.tx.send(SnapMsg::Poll(snap));
                }
            }
            if visible {
                was_visible = true;
            } else if was_visible {
                trim_working_set();
                was_visible = false;
                prev = None; // 再次显示时强制推送快照
            }
            std::thread::sleep(if visible { POLL_INTERVAL } else { IDLE_INTERVAL });
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
        working: Arc::new(AtomicBool::new(false)),
        snap: Arc::new(Mutex::new(Snapshot::default())),
        exit: Arc::new(ExitState::default()),
        tx,
    };

    // 初始快照：UI 线程直接写 Signal（无需走 channel），同时填好权威副本——
    // 托盘菜单/关闭拦截在首个轮询周期到来前就可能被触发。
    if let Some(m) = &mgr {
        let snap = compute_snapshot(m);
        ui.publish(&snap);
        ui.apply_snapshot(&snap);
    } else if let Some(de) = &detect_error {
        status.set("便携模式不可用".into());
        detail.set(de.clone());
    }

    // 后台轮询线程（可见时每 600ms，收进托盘后降到 800ms 只维护权威副本）。
    if let Some(m) = mgr {
        spawn_bg_poller(ui.clone(), m, title.clone());
    }

    let tray = build_tray(&ui);
    let content = build_content(&ui);
    // 关闭拦截：标题栏 × 收进托盘，托盘「退出」走卸载确认（两者同经 WM_CLOSE）。
    let u_close = ui.clone();
    // 托盘菜单启用态同步：WM_TIMER 隐藏态照常触发，把权威快照写进 en_* Signal，
    // 弹出的托盘菜单才拿得到当前状态（见 Ui::refresh_enables）。仅值变化时写，空闲不重绘。
    let u_tick = ui.clone();
    app.tray(tray)
        .content(content)
        .on_interval(TRAY_ENABLE_SYNC, move || u_tick.refresh_enables())
        .on_close_request(move || u_close.close_clicked())
        .run();
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
    let u_exit = ui.clone();
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
            // `.enabled(Signal)` 由 `on_interval → refresh_enables` 在 UI 线程周期刷新
            // （WM_TIMER 隐藏态照常触发），弹出时 windui 实时读该 Signal 决定灰显。
            // 点击回调再用 `desired_enables` 校验一次并给气泡：定时器最长滞后一拍，
            // 且 Signal 与真值理论上可能瞬时错位，兜底防误触发。
            TrayMenuItem::item("启动服务", move |ctx| {
                if u_start.can_start() {
                    u_start.start_clicked();
                } else {
                    ctx.notify(&u_start.title, &u_start.blocked_reason("服务已在运行"));
                }
            })
            .enabled(ui.en_start),
            TrayMenuItem::item("停止服务", move |ctx| {
                if u_stop.can_stop() {
                    u_stop.stop_clicked();
                } else {
                    ctx.notify(&u_stop.title, &u_stop.blocked_reason("服务未在运行"));
                }
            })
            .enabled(ui.en_stop),
            TrayMenuItem::item("打开设置", move |ctx| {
                if u_set.desired_enables().2 {
                    u_set.settings_clicked();
                } else {
                    ctx.notify(&u_set.title, &u_set.blocked_reason("请先启动服务再打开设置"));
                }
            })
            .enabled(ui.en_settings),
            TrayMenuItem::item("数据目录", move |_| u_data.data_clicked()).enabled(ui.en_data),
            TrayMenuItem::separator(),
            // 经 WM_CLOSE 汇入 on_close_request，与标题栏 × 同一条卸载确认链。
            TrayMenuItem::item("退出", move |_| u_exit.request_exit()),
        ])
}

/// 构建窗口控件树：TAB（运行 / 部署）。
/// 去掉了旧版"隐藏 visible_when 同步器"——Signal::set() 自动触发脏区，无需该 hack。
fn build_content(ui: &Ui) -> Element {
    // Signal<bool> 是 Copy，action_button 直接持有，无需 Rc 包装。
    // `on` 接收 `&mut EventCtx`：需要弹原生对话框的动作（部署/更新）经
    // `ctx.defer_blocking` 延后执行，不需要的动作直接忽略这个参数即可。
    let action_button = |label: &str, en: Signal<bool>, on: Box<dyn Fn(&mut EventCtx)>| {
        Element::button(label)
            .enabled(en)
            .weight(1.0)
            .height(30)
            .on_click(move |ctx| on(ctx))
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
                    Box::new(move |_ctx| u_start.start_clicked()),
                ))
                .child(action_button(
                    "停止服务",
                    ui.en_stop,
                    Box::new(move |_ctx| u_stop.stop_clicked()),
                )),
        )
        .child(
            row()
                .child(action_button(
                    "打开设置",
                    ui.en_settings,
                    Box::new(move |_ctx| u_set.settings_clicked()),
                ))
                .child(action_button(
                    "打开数据目录",
                    ui.en_data,
                    Box::new(move |_ctx| u_data.data_clicked()),
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
                    Box::new(move |ctx| u_update.update_clicked(ctx)),
                ))
                .child(action_button(
                    "复制到目录",
                    ui.en_deploy,
                    Box::new(move |ctx| u_dcopy.deploy_copy_clicked(ctx)),
                ))
                .child(action_button(
                    "从 ZIP 部署",
                    ui.en_deploy,
                    Box::new(move |ctx| u_dzip.deploy_zip_clicked(ctx)),
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
