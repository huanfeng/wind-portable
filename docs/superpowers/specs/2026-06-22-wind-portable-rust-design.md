# wind_portable（Rust + windui）重写设计

日期：2026-06-22
分支：`feat/wind-portable-rust`（worktree `WindInputPlus-portable`）

## 目标

把原 C# WinForms 绿色版/便携启动器 `WindInput/wind_portable/` 用 Rust +
[windui](https://github.com/huanfeng/wind-ui-rust) GUI 框架重写，作为
WindInputPlus 的独立项目 `wind_portable/`，交叉编译产物 `wind_portable.exe`。

便携启动器的职责：在任意非系统保护目录就地运行整套输入法（无需安装），负责
注册/注销 TSF、启停服务、开机自启、与已安装版的冲突检测，并提供托盘 + 窗口界面
与 CLI。

## 已确认决策

| 决策 | 选择 |
|---|---|
| 放置 | 独立 Cargo 项目 `WindInputPlus/wind_portable/`（自成 workspace，镜像原 C# 同级布局） |
| 首版范围 | **核心启动器优先**；zip/目录部署与在线更新**延后到第二阶段** |
| GUI 框架 | windui，**git 依赖跟 main 最新**（`git = "https://github.com/huanfeng/wind-ui-rust", branch = "main"`）。crates.io 上无 `windui` 包 |
| 停止服务 | **终止进程**（TerminateProcess by path）；RPC 客户端**预留 shutdown 接口**：停止时先尝试 RPC 优雅关闭→失败回退终止。主仓库后续加 `system.shutdown` 后零改动联调 |
| 副作用范围 | 自启注册表 + UAC 提权 + icacls 授权 + regsvr32 + InstallLayoutOrTip + 托盘**全部实现**；Linux 仅交叉编译验证，注册/提权/托盘/进程操作需 **Windows 实测** |
| 构建变体 | **运行时探测**（同 C#）：扫描 launcher 旁是否存在 `wind_input_debug.exe` 决定 debug/release 变体；launcher 本身单一二进制，不走 cargo feature |

## 与新 Rust 服务的契约差异（相对 C# 版）

- C# 用 RPC 方法 `System.Ping` / `System.Shutdown`。新 Rust 服务（`wind-rpc`）方法为
  `system.status` / `system.info` / `config.*`，**无 ping、无 shutdown**。
  - 存活探测 → 调 `system.status`，连上并成功返回即"运行中"。
  - 优雅关闭 → 暂无；走终止进程（见上）。
- 管道端点：控制通道 `\\.\pipe\wind_input{suffix}_ctrl`（suffix：release=""，debug="_debug"）。
- 帧格式：4 字节大端 uint32 长度前缀 + JSON 载荷（与 `wind-ipc` 一致）。
- 请求体：`{"version":1,"id":<n>,"method":"<m>","params":{}}`；响应含 `result` 或 `error`。

## 常量来源（硬编码于 `variant.rs`，照搬 C# BuildVariant / wind_tsf Globals）

| 项 | release | debug |
|---|---|---|
| CLSID | `{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}` | `{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}` |
| ProfileStr | `0804:{99C2EE30...}{99C2EE31...}` | `0804:{99C2DEB0...}{99C2DEB1...}` |
| 服务 exe | `wind_input.exe` | `wind_input_debug.exe` |
| TSF dll | `wind_tsf.dll` / `wind_tsf_x86.dll` | `wind_tsf_debug.dll` / `wind_tsf_debug_x86.dll` |
| 自启键名 | `WindInput` | `WindInputDebug` |
| 显示名 | `清风输入法` | `清风输入法开发版` |
| 数据目录名 | `WindInput` | `WindInputDebug` |
| Mutex | `Local\WindPortableLauncher` | `Local\WindPortable_debugLauncher` |
| ShowEvent | `Local\WindPortableShowEvent` | `Local\WindPortable_debugShowEvent` |
| 便携 marker | `wind_portable_mode` | 同 |
| 便携数据目录 | `userdata/` | 同 |

## 架构与模块

独立 crate，目录：

```
wind_portable/
├── Cargo.toml
├── src/
│   ├── main.rs           # 入口：CLI 解析 → (CLI 命令直跑 | 单实例+GUI)
│   ├── variant.rs        # 运行时变体探测 + 全部常量（CLSID/Profile/文件名/管道/目录/Mutex）
│   ├── layout.rs         # PortableConfig：探测 service/setting exe、TSF dll、userdata、marker、保护目录判定
│   ├── rpc.rs            # 极简 RPC 客户端：system.status 探活；shutdown 预留（先 RPC 后回退）
│   ├── process.rs        # 按路径查/终止进程（OpenProcess/QueryFullProcessImageNameW/TerminateProcess）
│   ├── registration.rs   # regsvr32(x64+x86)、InstallLayoutOrTip(input.dll 动态加载)、icacls 授权、UAC 提权、冲突检测
│   ├── service.rs        # ServiceManager：start/stop/status/openSettings/openUserdata + 自启注册表
│   ├── singleton.rs      # 单实例 Mutex + ShowEvent（唤起已有窗口）
│   ├── cli.rs            # CLI 选项解析与执行（-start/-stop/-status/-settings/-userdata/-elevate-*）
│   └── ui/
│       ├── mod.rs        # windui App + 控件树（状态标签 + 6 按钮）+ 轮询器（visible_when 每帧钩子）
│       └── tray.rs       # 托盘构建（显示/隐藏/退出）
└── res/                  # 图标（可选，缺省用纯色）
```

平台无关纯逻辑（`variant` 常量、`layout` 路径探测、`cli` 解析、`rpc` 帧编解码、
路径压缩显示）可在 Linux 跑单元测试。Windows 副作用模块在非 Windows 下以 `#[cfg]`
退化为编译占位（保证 `cargo check` 过），真实路径仅在 `cfg(windows)` 编译。

## 依赖

```toml
[dependencies]
windui = { git = "https://github.com/huanfeng/wind-ui-rust", branch = "main" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"

[target.'cfg(windows)'.dependencies]
windows = { version = "0.62", features = [
  "Win32_Foundation", "Win32_System_Threading", "Win32_System_Registry",
  "Win32_Security", "Win32_UI_Shell", "Win32_System_Com",
  "Win32_System_LibraryLoader", "Win32_Storage_FileSystem",
] }
```

`InstallLayoutOrTip` 不在 windows-rs 绑定 → 用 `LoadLibraryW("input.dll")` +
`GetProcAddress("InstallLayoutOrTip")` 动态调用（签名 `fn(*const u16, u32) -> BOOL`）。
ACL 授权（ALL_APP_PACKAGES，SID `S-1-15-2-1`）走 `icacls "<dll>" /grant *S-1-15-2-1:(RX)`
子进程，避免直接拼 SetNamedSecurityInfo。注册表读写用 windows-rs `RegOpenKeyExW` 等
（不引入 winreg，减少依赖）。

## 关键设计：UI 实时状态刷新（windui 约束下的方案）

windui 是单线程 retained 模式：`App` 仅接受 `content(Element)`，**无 app 级帧/idle 回调、
无定时器、无跨线程唤醒**；`request_repaint()` 是 thread-local；状态绑定 `Rc<RefCell>` 为
`!Send`，后台线程无法持有。因此不能用"后台线程轮询 + marshal 回 UI"的常规做法。

**采用的机制（不改 windui）**：

- 反应式状态用 windui 自带绑定：状态文字 `label_rc(Rc<RefCell<String>>)`；每个按钮
  `enabled(Rc<Cell<bool>>)` 运行期启停；目录提示等同理。
- 一个隐藏的"轮询器"元素 `Element::leaf().visible_when(closure)`。其谓词在每帧
  layout/paint 遍历时于 **UI 线程**被调用（见 windui `core.rs:100`）——这是逐帧钩子。
  谓词内：按 `Instant` 节流（稳态 ~1s，操作后冷却期 ~300ms 持续 ~15s），到点则做一次
  **同步刷新**（RPC `system.status` + 注册表冲突/注册态 + 部署源探测，均为毫秒级），把结果
  写入上述绑定状态；只要处于"活跃轮询窗口"且窗口可见，就 `anim::request_repaint()` 续帧。
- 续帧门控：托盘隐藏/显示翻转 `Rc<Cell<bool>>` 可见标志；隐藏时谓词不再续帧 →
  事件循环回到 `GetMessageW` 阻塞、零 CPU；显示时由 WM_PAINT 重启循环。

**已知取舍**：窗口可见且处于活跃轮询窗口期间，按帧率（≤60fps）重绘静态窗口，有少量
CPU 开销。对一个通常隐藏到托盘、短时操作的小启动器可接受；稳态（无操作 15s 后）回到
事件驱动 + 零空闲。同步 RPC 在服务启动瞬间极端情况下可能有一次 ≤200ms 卡顿（管道存在
但 busy），罕见且短暂；RPC 超时设 200ms 上限。

## 运行流程

- **CLI 路径**：含 `-start/-stop/-status/-settings/-userdata/-elevate-register/-elevate-unregister`
  且无 `-ui` → 直接走 `cli::run`（ServiceManager 命令、打印结果、退出），不起 GUI。检测失败
  且有动作且非 UI → stderr 报错 + exit 1。
- **GUI 路径**：占用单实例 Mutex；占用失败 → Set ShowEvent 唤起已有窗口后退出；否则
  建 windui 窗口 + 托盘，启动轮询器；首帧即触发一次刷新。
- **start**：冲突检查 → 建 userdata 目录树 + marker → 若未注册则 `register()`（管理员则直接
  regsvr32+InstallLayoutOrTip+icacls；否则 ShellExecuteEx runas 自我重启带 `-elevate-register`）→
  写自启注册表 → 启动 service 进程（隐藏窗口）。
- **stop**：写 stopped marker → 先试 RPC shutdown（当前必失败→忽略）→ 回退终止 service 进程 →
  删自启注册表 → 若已注册则 `unregister()`。
- **退出（托盘）**：若服务运行/已注册，确认后异步 StopService 再退出进程；否则直接退出。

## 冲突检测（照搬 C# RegistrationManager）

1. 当前目录是安装版目录（NSIS `Uninstall\<显示名>\InstallLocation` 匹配，或同目录有
   `uninstall.exe`）→ 禁用便携模式。
2. 其他位置已注册 TSF DLL（读 `CLSID\{clsid}\InprocServer32`）：
   - 注册路径 == 本便携 DLL → 无冲突。
   - 注册文件已不存在 → 残留，可接管。
   - 注册 DLL 同级有 `wind_portable_mode` 标记（另一便携实例）：服务未运行→可接管；
     运行中→提示先停止。
   - 否则来自安装版→禁用便携模式。

## 测试策略

- **首要门槛**：Linux 下 `cargo xwin build --target x86_64-pc-windows-msvc` 通过。
- **单元测试**（平台无关，Linux 跑 `cargo test`）：变体探测、PortableConfig 路径探测
  （用临时目录布置假 exe）、CLI 解析、RPC 请求/响应帧编解码、CompactPath 路径压缩、冲突
  判定纯逻辑。
- **Windows 实测清单**（需设备，文档化）：regsvr32 注册/注销、InstallLayoutOrTip
  安装/卸载、UAC 提权弹窗、icacls 授权、进程终止、命名管道探活、托盘显示/隐藏/退出、
  自启注册表读写、单实例唤起、安装版冲突禁用。

## 构建集成

`scripts/dev.sh` 新增 `build_portable()`：`cargo xwin build --release --target $TARGET`，
产物 `wind_portable.exe` 拷到 `build/` 与 `build_debug/`（同一份二进制，运行时自辨变体）。

## 分阶段实现

1. 脚手架 + `variant` + `layout` + 单测（交叉编译过）。
2. `rpc` + `process` + `cli` 解析 + 单测。
3. `registration` + `service`（注册/提权/自启/启停）。
4. `singleton` + `main` CLI 派发整合。
5. `ui`（窗口 + 托盘 + 轮询器 + 绑定）。
6. `dev.sh build_portable` + Windows 实测清单文档。

每阶段：交叉编译验证 + 纯逻辑单测，提交一次。

## 风险

1. windui 跟 main：上游一改可能突然编译失败 → CI/构建固定先 `cargo update -p windui`
   核验；失败时回退到可用 rev。
2. windui 某些控件/方法签名随 main 漂移 → 实现时以本地 `wind-ui-rust` 当前 HEAD 源码为准。
3. `visible_when` 谓词每帧多次调用（layout/paint/命中各一次）→ 轮询器用 `Instant` 节流，
   保证同帧多次调用幂等。
4. Windows 副作用无法在 Linux 验证 → 严格代码审查 + 设备实测清单兜底。
