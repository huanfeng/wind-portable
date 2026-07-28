# 贡献指南

感谢您对清风输入法便携启动器 (wind-portable) 的关注！我们欢迎所有形式的贡献，包括 Bug 报告、功能建议和代码提交。

本仓库是清风输入法 (WindInput) 的组件之一，负责**免安装便携运行**：注册/注销 TSF、
启停服务、开机自启、冲突检测、部署与在线更新。输入法核心引擎在主仓库
[WindInput](https://github.com/huanfeng/WindInput)——引擎、词库、候选、主题相关的问题请到主仓库反馈。

> **⚠️ alpha 阶段 PR 政策**
>
> 本项目目前处于 alpha 阶段，代码与文档变动频繁。为降低维护与冲突成本，**此阶段暂不接受仅包含文档改动或轻微改动的 Pull Request**，例如：
>
> - 纯文档变更（错别字、措辞润色、README/注释更新等）
> - 轻微改动（个别字符串、代码格式调整、无功能影响的小修小补等）
>
> 如发现文档错误或有改进建议，欢迎通过 [Issue](../../issues) 反馈，由维护者统一处理。功能性的 Bug 修复与新特性 PR 不受此限制，仍然欢迎提交。

## 签署 CLA（必须）

**所有贡献者在首次提交 Pull Request 前必须签署贡献者许可协议 (CLA)。**

这是为了确保项目的许可证管理和知识产权的一致性。流程如下：

1. 提交您的 Pull Request
2. CLA Assistant 机器人会自动在 PR 中发起签署请求
3. 在 PR 评论中回复：`I have read the CLA Document and I hereby sign the CLA`
4. 签署完成后，CLA 检查将自动通过

未签署 CLA 的 PR 将无法合并。完整协议内容请参阅 [CLA.md](CLA.md)。

## Bug 报告

请通过 [GitHub Issues](../../issues) 的 **Bug 反馈** 模板提交，并尽量包含以下信息：

- 操作系统与版本（如 Windows 11 24H2）
- 启动器版本、便携目录位置（是否位于 `Program Files` 等受保护路径）
- 是否与已安装版共存
- 重现步骤、预期行为与实际行为
- 相关日志：便携版日志位于 `<程序目录>\userdata\logs\`

`wind_portable -status` 的输出（形如 `service=running mode=conflict reason="..."`）
对定位启停与冲突类问题很有帮助，请一并附上。

## 功能建议

欢迎通过 [GitHub Issues](../../issues) 的 **功能建议** 模板提交。请描述您希望实现的功能、使用场景，如有参考实现请提供链接。

## 代码贡献

### 开发环境

- Rust stable 工具链（rustup 安装，含 `rustfmt` 与 `clippy` 组件）
- **Windows 本机构建**：Visual Studio 2022 生成工具（MSVC 链接器）
- **Linux/macOS 交叉编译**：[cargo-xwin](https://github.com/rust-cross/cargo-xwin)
  （`cargo install cargo-xwin`）与 LLVM 工具链（clang / lld / llvm）

本项目**只面向 Windows 目标**。`Cargo.toml` 把 GUI 与系统调用相关依赖放在
`[target.'cfg(windows)'.dependencies]` 下，因此非 Windows 主机上 `cargo test`
可以直接跑纯逻辑单元测试（变体探测、布局、CLI 解析、RPC 帧、部署逻辑）而不拉入 GUI 框架。

### 构建

| 场景 | 命令 |
|------|------|
| Windows 本机 Release | `cargo build --release` |
| Linux/macOS 交叉编译 | `cargo xwin build --release --target x86_64-pc-windows-msvc` |

产物位于 `target/release/wind_portable.exe` 或 `target/<target>/release/wind_portable.exe`。

版本号可经 `WIND_APP_VERSION` 环境变量注入（主仓打包脚本与 CI 由 `docs/VERSION` 统一注入），
未注入时回退到 `CARGO_PKG_VERSION`。

> **本仓不单独发版。** 启动器随 WindInput 的安装包与便携包一起分发，由主仓的
> [Release 流程](https://github.com/huanfeng/WindInput/releases)以原生 MSVC 构建并打包
> （资源嵌入依赖 Windows 的 `rc.exe`，交叉编译链只作校验用途）。
> 因此本仓只有 CI 门禁，没有 release workflow，也不打 tag。

### 提 PR 前的自检

CI（`.github/workflows/ci.yml`）在 ubuntu 上跑以下三项，请在本地先跑一遍：

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings    # 交叉编译时：cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings
cargo test
```

`cargo test` 只覆盖平台无关的纯逻辑。**注册表、UAC 提权、TSF 注册、服务启停、托盘等
Windows 副作用无法在 CI 中验证**，涉及这些路径的改动请在 Windows 设备上按
[`docs/wind_portable-windows-test-checklist.md`](docs/wind_portable-windows-test-checklist.md)
实测，并在 PR 中说明测试结果。

### Git Hooks（首次克隆后建议激活）

仓库自带 `.githooks/pre-commit`（提交前自动跑 `cargo fmt --check`，避免未格式化代码被提交后才在 CI 里暴露），默认不生效，需一次性激活：

```bash
git config core.hooksPath .githooks
```

（仅影响本地 clone，不会随仓库自动传播。）

### 提交规范

本项目使用 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/) 规范：

```
<类型>(<范围>): <描述>

[可选的正文]
```

类型包括：

| 类型 | 说明 |
|------|------|
| `feat` | 新功能 |
| `fix` | Bug 修复 |
| `docs` | 文档变更 |
| `refactor` | 代码重构（不改变行为） |
| `perf` | 性能优化 |
| `test` | 测试相关 |
| `chore` | 构建/工具变更 |
| `style` | 格式化（如 `cargo fmt`） |

范围示例：`ui`、`deploy`、`rpc`、`reg`、`service`、`cli`、`portable`、`res`、`build`、`deps`

### 代码风格

- 必须使用 `cargo fmt` 格式化（CI 会检查）；**逻辑修改与 fmt 修改分开提交**
- 新增的 Windows 副作用路径请用 `#[cfg(windows)]` 隔离，保持非 Windows 主机上 `cargo test` 可跑
- 复杂决策请在代码注释中写明**为什么**——本项目大量涉及 Win32 与 TSF 的非直觉行为，
  现有注释（如 `build.rs` 中关于 manifest 与 `TaskDialogIndirect` 的说明）即是范例

### Pull Request 流程

1. Fork 本仓库并从 `main` 分支创建您的分支
2. 完成修改后运行上面「提 PR 前的自检」的三条命令
3. 涉及 Windows 副作用的改动，按测试清单在真实设备上实测
4. 按 PR 模板填写变更说明、测试情况与检查清单
5. 提交 PR 并等待 CLA 检查和代码审查
6. 根据审查意见修改后，PR 将被合并

## 项目结构

| 文件 / 目录 | 说明 |
|------|------|
| `src/main.rs` | 入口：CLI 分发与 GUI 启动 |
| `src/cli.rs` | 命令行参数解析 |
| `src/variant.rs` | 运行时 debug/release 变体探测与常量（CLSID、管道名、服务 exe 名等） |
| `src/layout.rs` | 便携布局探测（PortableConfig）：定位服务/设置 exe、TSF DLL、userdata、便携标记 |
| `src/registration.rs` | TSF 注册/注销、UAC 提权自我拉起 |
| `src/service.rs` / `src/process.rs` | 服务启停与进程操作 |
| `src/reg.rs` | 注册表访问（开机自启等） |
| `src/rpc.rs` | 与输入法服务的命名管道 RPC 客户端 |
| `src/deploy.rs` | 复制部署、ZIP 部署与在线更新 |
| `src/singleton.rs` | 单实例互斥 |
| `src/ui/` | 托盘与窗口界面（windui） |
| `build.rs` | 嵌入图标、版本信息与应用清单 |
| `docs/` | 设计文档与 Windows 测试清单 |

## 许可证

提交贡献即表示您同意您的贡献将按照项目的 [MIT 许可证](LICENSE) 进行授权。
