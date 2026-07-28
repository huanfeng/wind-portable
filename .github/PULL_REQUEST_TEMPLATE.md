## 变更说明

<!-- 简要描述这个 PR 做了什么 -->

## 变更类型

- [ ] Bug 修复
- [ ] 新功能
- [ ] 重构 / 代码优化
- [ ] 文档更新
- [ ] 构建 / CI 相关
- [ ] 其他（请说明）

## 相关 Issue

<!-- 如有关联的 Issue，请填写，例如：Fixes #123 -->

## 测试情况

<!-- 描述你做了哪些测试来验证这个变更 -->

- [ ] `cargo fmt --all -- --check` 通过
- [ ] `cargo clippy --all-targets -- -D warnings` 通过
- [ ] `cargo test` 通过（平台无关的纯逻辑）
- [ ] 已在 Windows 10/11 实测（涉及注册/启停/自启/托盘/部署等副作用时**必须**勾选）

<!-- 涉及 Windows 副作用时，请说明按 docs/wind_portable-windows-test-checklist.md 覆盖了哪些项 -->

## 检查清单

- [ ] 代码已用 `cargo fmt` 格式化（逻辑修改与 fmt 修改分开提交）
- [ ] 新增的 Windows 副作用路径已用 `#[cfg(windows)]` 隔离，非 Windows 主机仍可 `cargo test`
- [ ] 提交信息遵循 [Conventional Commits](https://www.conventionalcommits.org/) 规范
- [ ] 已阅读 [贡献指南](../CONTRIBUTING.md)
- [ ] 首次贡献已签署 [CLA](../CLA.md)
