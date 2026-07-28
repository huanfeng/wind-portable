# wind_portable 设计说明

> 本文源自 2026-06-22 的 Rust 重写设计稿（原 C# WinForms 便携启动器的替代方案），
> 已按当前实现订正。纯规划性内容（分阶段实施计划、初版风险清单）不再保留。

## 职责

在任意非系统保护目录**就地运行**整套输入法（无需安装）：注册/注销 TSF、启停服务、
开机自启、与已安装版的冲突检测、便携包部署与在线更新，并提供托盘 + 窗口界面与 CLI。

单一二进制，**运行时**自辨 debug/release 变体（扫描旁置的 `wind_input_dev.exe`），
两种变体共用同一份 exe，不走 cargo feature。

## 与输入法服务的契约

- **存活探测**：RPC `system.status`（服务端方法为 `system.status` / `system.info` /
  `config.*`，无 `System.Ping`）。连上并成功返回即视为运行中。
- **优雅关闭**：服务端暂无 `system.shutdown`，停止走**终止进程**（按可执行文件全路径匹配，
  避免误杀其他目录下的同名便携实例）。RPC shutdown 已预留（`rpc::try_shutdown`），
  主仓补上该方法后零改动联调。
- **端点**：控制通道 `\\.\pipe\wind_input{suffix}_ctrl`（suffix：release 为空，dev 为 `_dev`）。
- **帧格式**：4 字节大端 uint32 长度前缀 + JSON 载荷（与 `wind-ipc` 一致）。
- **请求体**：`{"version":1,"id":<n>,"method":"<m>","params":{}}`；响应含 `result` 或 `error`。

## 变体常量

硬编码于 `variant.rs`，与 C# `BuildVariant` / `wind_tsf` Globals 对齐：

| 项 | release | dev |
|---|---|---|
| CLSID | `{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}` | `{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}` |
| ProfileStr | `0804:{99C2EE30...}{99C2EE31...}` | `0804:{99C2DEB0...}{99C2DEB1...}` |
| 服务 exe | `wind_input.exe` | `wind_input_dev.exe` |
| TSF dll | `wind_tsf.dll` / `wind_tsf_x86.dll` | `wind_tsf_dev.dll` / `wind_tsf_dev_x86.dll` |
| 自启键名 | `WindInput` | `WindInputDev` |
| 显示名 | `清风输入法` | `清风输入法开发版` |
| 数据目录名 | `WindInput` | `WindInputDev` |
| 便携 marker | `wind_portable_mode` | 同 |
| 便携数据目录 | `userdata/` | 同 |

（以 `variant.rs` 的实际取值为准；本表为设计意图说明。）

## 模块划分

```
src/
├── main.rs           # 入口：CLI 解析 → (CLI 命令直跑 | 单实例 + GUI)
├── cli.rs            # CLI 选项解析与执行
├── variant.rs        # 运行时变体探测 + 全部常量（CLSID/Profile/文件名/管道/目录/Mutex）
├── layout.rs         # 便携布局探测（PortableConfig）：在 launcher 目录/工作目录及其上级中定位
│                     #   service/setting exe、TSF dll、userdata、便携标记、图标；保护目录判定
├── rpc.rs            # 极简 RPC 客户端：system.status 探活；shutdown 预留（先 RPC 后回退）
├── process.rs        # Toolhelp 快照枚举 + QueryFullProcessImageNameW，按全路径精确查/终止进程
├── reg.rs            # 注册表读写助手（windows-rs，locale 无关，不解析 reg.exe 的本地化输出）
├── registration.rs   # regsvr32(x64+x86)、InstallLayoutOrTip、icacls 授权、UAC 提权、冲突检测
├── service.rs        # ServiceManager：start/stop/status/openSettings/openUserdata + 自启注册表
├── deploy.rs         # 便携包部署与在线更新（平台无关，可 Linux 单测）
├── dialog.rs         # 文件/目录选择（windui PickDialog）与消息框（MessageBoxW）
├── singleton.rs      # 单实例 Mutex + 唤起已有窗口
└── ui/mod.rs         # windui App + 控件树 + 托盘 + 轮询器
```

平台无关的纯逻辑（`variant` 常量、`layout` 路径探测、`cli` 解析、`rpc` 帧编解码、
`deploy` 部署逻辑、路径压缩显示）可在 Linux 跑单元测试。Windows 副作用模块以
`#[cfg(windows)]` 隔离，非 Windows 主机上退化为编译占位。

依赖同样按平台分层：`windui` / `windows` / `ico` 放在
`[target.'cfg(windows)'.dependencies]`，因此 Linux 主机 `cargo test` 不会拉入 GUI 框架。

`InstallLayoutOrTip` 不在 windows-rs 绑定中 → 用 `LoadLibraryW("input.dll")` +
`GetProcAddress("InstallLayoutOrTip")` 动态调用（签名 `fn(*const u16, u32) -> BOOL`）。
ACL 授权（ALL_APP_PACKAGES，SID `S-1-15-2-1`）走 `icacls "<dll>" /grant *S-1-15-2-1:(RX)`
子进程，避免直接拼 `SetNamedSecurityInfo`。

## 关键设计：UI 实时状态刷新

windui 是单线程 retained 模式：`App` 仅接受 `content(Element)`，**无 app 级帧/idle 回调、
无定时器、无跨线程唤醒**；`request_repaint()` 是 thread-local；状态绑定 `Rc<RefCell>` 为
`!Send`，后台线程无法持有。因此不能用"后台线程轮询 + marshal 回 UI"的常规做法。

**采用的机制（不改 windui）**：

- 反应式状态用 windui 自带绑定：状态文字 `label_rc(Rc<RefCell<String>>)`；每个按钮
  `enabled(Rc<Cell<bool>>)` 运行期启停。
- 一个隐藏的"轮询器"元素 `Element::leaf().visible_when(closure)`。其谓词在每帧
  layout/paint 遍历时于 **UI 线程**被调用——这是逐帧钩子。谓词内按 `Instant` 节流
  （稳态约 1s，操作后冷却期约 300ms 持续约 15s），到点则做一次**同步刷新**
  （RPC `system.status` + 注册表冲突/注册态 + 部署源探测，均为毫秒级），把结果写入绑定状态；
  只要处于"活跃轮询窗口"且窗口可见，就 `anim::request_repaint()` 续帧。
- 续帧门控：托盘隐藏/显示翻转 `Rc<Cell<bool>>` 可见标志；隐藏时谓词不再续帧 →
  事件循环回到 `GetMessageW` 阻塞、零 CPU；显示时由 WM_PAINT 重启循环。

**已知取舍**：窗口可见且处于活跃轮询窗口期间，按帧率（≤60fps）重绘静态窗口，有少量
CPU 开销。对一个通常隐藏到托盘、短时操作的小启动器可接受；稳态（无操作约 15s 后）回到
事件驱动 + 零空闲。同步 RPC 在服务启动瞬间极端情况下可能有一次 ≤200ms 卡顿（管道存在
但 busy），罕见且短暂；RPC 超时设 200ms 上限。

隐藏态下托盘菜单项的启停状态不依赖绘制帧（隐藏窗口收不到 WM_PAINT），另有独立的同步路径。

## 运行流程

- **CLI 路径**：含 `-start/-stop/-status/-settings/-userdata/-elevate-register/-elevate-unregister`
  且无 `-ui` → 直接走 `cli::run`（执行命令、打印结果、退出），不起 GUI。探测失败且有动作且
  非 UI → stderr 报错 + exit 1。
- **GUI 路径**：占用单实例 Mutex；占用失败 → 找到首实例窗口并前置后退出（后启动者本就持有
  用户输入前台权限，`SetForegroundWindow` 可靠生效，无需事件 + 等待线程，规避 windui
  单线程模型下的跨线程唤醒难题）；否则建窗口 + 托盘，启动轮询器，首帧即触发一次刷新。
- **start**：冲突检查 → 建 userdata 目录树 + marker → 若未注册则 `register()`（管理员则直接
  regsvr32 + InstallLayoutOrTip + icacls；否则 `ShellExecuteEx runas` 自我重启并带
  `-elevate-register`）→ 写自启注册表 → 启动 service 进程（隐藏窗口）。
- **stop**：写 stopped marker → 先试 RPC shutdown（当前必失败→忽略）→ 回退终止 service 进程 →
  删自启注册表 → 若已注册则 `unregister()`。
- **退出（托盘）**：若服务运行/已注册，确认后异步停止服务再退出进程；否则直接退出。

## 冲突检测

照搬 C# `RegistrationManager` 的判定顺序：

1. 当前目录是安装版目录（NSIS `Uninstall\<显示名>\InstallLocation` 匹配，或同目录有
   `uninstall.exe`）→ 禁用便携模式。
2. 其他位置已注册 TSF DLL（读 `CLSID\{clsid}\InprocServer32`）：
   - 注册路径 == 本便携 DLL → 无冲突。
   - 注册文件已不存在 → 残留，可接管。
   - 注册 DLL 同级有 `wind_portable_mode` 标记（另一便携实例）：服务未运行 → 可接管；
     运行中 → 提示先停止。
   - 否则来自安装版 → 禁用便携模式。

## 部署与在线更新

`deploy.rs`，平台无关（`std::fs` + 纯 Rust deflate 后端的 `zip`），可在 Linux 单测。

- **ZIP 部署/更新**：先 `validate_zip` 校验包内必须含服务 exe 与 TSF DLL（按文件名，
  大小写不敏感），再 `deploy_from_zip` **原子替换**——每个文件先完整写到临时文件
  `*.new_<n>` 再 `rename` 就位，写入中途失败时目标保持原样，绝不留下截断的 exe；
  目标被占用（运行中 exe / 已加载 DLL：不可覆盖但可改名）时先把旧文件改名 `*.old_<n>`。
  返回 `needs_restart` 表示是否替换了正在运行的自身 exe。
- **目录复制部署**：跳过 `userdata/`（用户数据不随便携包分发）与**根级** `uninstall.exe`。
  后者是安装版专属，若被带到目标目录，会让目标在便携运行时被 `registration` 的
  `is_installed_directory` 误判为"安装目录"而禁用便携模式——"把安装版复制到 U 盘当便携用"
  正是栽在这里：复制看似成功，到了别处却打不开。
- **残留清理**：启动时清理本模块生成的 `*.old_<数字>` / `*.new_<数字>` / `*.bak`。
  用精确后缀（数字）而非 `.old_` 子串匹配，避免误删形如 `notes.old_draft.txt` 的用户文件。

## 资源嵌入

`build.rs` 用 `winresource`（与 `wind_input` 同款）嵌入图标、版本信息与应用清单。

`res/app.ico` 由主仓 WindInput 的图标转换生成（项目自有素材），含 48×48 / 32×32 / 16×16
三档 32bpp。
图标资源 ID 必须钉死为 `1`——windui 的窗口类用 `LoadIcon(hinst, MAKEINTRESOURCE(1))` 取它，
故 `build.rs` 用 `set_icon_with_id(&ico, "1")` 而非依赖 `set_icon` 的默认 ID。
同一份 .ico 还会被 `ui/mod.rs` 以 `include_bytes!` 内嵌、经 `ico` crate 解码为 RGBA 供托盘使用。

清单只声明 Common-Controls 6.0.0.0，**故意不声明 dpiAware**——windui 在运行时调
`SetProcessDpiAwarenessContext(PER_MONITOR_V2)`，而 manifest 中的 DPI 声明优先级更高，
一旦写上会锁死并使 windui 的运行时设置失效。

清单缺失是**致命**的：windui 静态导入 comctl32 v6 的 `TaskDialogIndirect`，无
Common-Controls 6.0 清单时加载器会绑到 comctl32 v5（不导出该函数），exe 根本无法启动。
因此发布链应把 `build.rs` 的资源嵌入警告视为构建失败对待。

版本号优先级：`WIND_APP_VERSION`（主仓打包脚本/CI 从 `docs/VERSION` 注入）
> `{manifest}/../docs/VERSION` > `CARGO_PKG_VERSION`。

## 测试策略

- **单元测试**（平台无关，Linux 跑 `cargo test`）：变体探测、`PortableConfig` 路径探测
  （用临时目录布置假 exe）、CLI 解析、RPC 请求/响应帧编解码、路径压缩、部署逻辑。
- **交叉编译门槛**：`cargo xwin build --target x86_64-pc-windows-msvc` 通过（CI 门禁）。
- **Windows 实测**：regsvr32 注册/注销、InstallLayoutOrTip、UAC 提权、icacls 授权、
  进程终止、命名管道探活、托盘、自启注册表、单实例唤起、安装版冲突禁用等副作用无法在
  CI 验证，清单见 [`wind_portable-windows-test-checklist.md`](wind_portable-windows-test-checklist.md)。
