# wind_portable — 清风输入法便携启动器（绿色版）

Rust + [windui](https://github.com/huanfeng/wind-ui-rust) 实现的免安装便携启动器，对应原
C# 版 `WindInput/wind_portable/`。在任意非系统保护目录就地运行整套输入法：注册/注销 TSF、
启停服务、开机自启、与已安装版的冲突检测，提供托盘 + 窗口界面与 CLI。

单一二进制，**运行时**自辨 debug/release 变体（扫描旁置的 `wind_input_dev.exe`），
release/debug 共用同一份 exe。

## 构建

```bash
# Windows 本机
cargo build --release

# 非 Windows 主机交叉编译到 Windows MSVC 目标
cargo build --release --target x86_64-pc-windows-msvc
```

产物位于 `target/<target>/release/wind_portable.exe` 或 `target/release/wind_portable.exe`
（release，LTO+strip，约 0.8MB）。

## CLI

```
wind_portable                 # 启动 GUI（默认）
wind_portable -start          # 注册 + 自启 + 启动服务
wind_portable -stop           # 停止服务 + 移除自启 + 注销
wind_portable -status         # 打印 service=running|stopped[ mode=conflict reason="..."]
wind_portable -settings       # 打开设置程序
wind_portable -userdata       # 打开数据目录
wind_portable -ui             # 强制 GUI（即使带了其他动作）
# 内部（由 UAC 提权自我拉起）：-elevate-register / -elevate-unregister
```

## 与新 Rust 服务的契约

- 存活探测：RPC `system.status`（无 `System.Ping`）。
- 优雅关闭：服务端暂无 `system.shutdown`；停止走**终止进程**。RPC shutdown 已预留
  （`rpc::try_shutdown`），主仓库补上该方法后零改动联调。
- 端点：`\\.\pipe\wind_input{suffix}_ctrl`；帧 = 4 字节大端长度 + JSON。

## 设计

见 [`docs/superpowers/specs/2026-06-22-wind-portable-rust-design.md`](docs/superpowers/specs/2026-06-22-wind-portable-rust-design.md)。
UI 实时刷新机制（windui 单线程约束下的隐藏轮询器 + active 窗口续帧）详见该文档"关键设计"节。

## 测试

- **纯逻辑单元测试**（平台无关，Linux 跑）：`cargo test`（变体/布局/CLI/RPC 帧/部署逻辑）。
- **Windows 副作用**：无法在 Linux 验证，需在 Windows 设备实测，清单见
  [`docs/wind_portable-windows-test-checklist.md`](docs/wind_portable-windows-test-checklist.md)。

## 状态

核心启动器 + 部署/在线更新完成（交叉编译通过 + 单测绿 + Windows clippy 干净），**待 Windows 设备实测**。
- 已实现：启停/注册/自启/冲突检测/托盘、ZIP 在线更新、复制部署、ZIP 部署、启动时清理旧文件。
- 待补：托盘菜单项置灰（windui 框架层支持后接入）、真实 .ico 图标。
