# wind_portable Windows 设备实测清单

Linux 仅能交叉编译 + 跑纯逻辑单测。以下副作用必须在 Windows 设备上人工验证。
建议在**非系统保护目录**（如 `D:\test\wind\`）放置便携包后逐项核对。

`wind_tsf.dll`（及可选 `wind_tsf_x86.dll`）、`data/`。dev 变体则为带 `_dev` 后缀的同名文件。

## 变体探测
- [ ] 目录无 `wind_input_dev.exe` → 标题为「清风输入法便携启动器」（release 变体）。
- [ ] 放入 `wind_input_dev.exe`（或 `build_dev/` 子目录）→ 标题带「(Dev)」，使用 `_dev` 管道/CLSID/数据目录。

## 布局探测与保护目录
- [ ] 把便携包整体复制到 `C:\Program Files\...` 运行 → 提示"系统保护目录，不支持便携模式"。
- [ ] exe 与服务在 `build/` 或 `build_dev/` 子目录时仍能正确定位。

## CLI（建议从 cmd 重定向查看输出，或确认退出码）
- [ ] `wind_portable -status` → `service=stopped`（未启动时）。
- [ ] `wind_portable -start` → 触发 UAC，注册成功后 `-status` 为 `service=running`。
- [ ] `wind_portable -stop` → `service stopped`，`-status` 回 `service=stopped`。
- [ ] `wind_portable -settings` → 打开设置程序。
- [ ] `wind_portable -userdata` → 资源管理器打开 `userdata\`。
- [ ] 探测失败 + 带动作 + 非 `-ui` → stderr 报错且退出码 1。

## TSF 注册/注销（核心副作用）
- [ ] 启动后注册表 `HKCU\Software\Classes\CLSID\{CLSID}\InprocServer32` 指向本便携 DLL。
- [ ] 输入法出现在系统输入法列表，可切换并正常输入中文。
- [ ] x86 DLL 存在时，32 位程序（如旧版 Office）中也能加载输入法。
- [ ] icacls 授权后，UWP/沙箱应用（如记事本 Store 版、Edge 地址栏）中可用。
- [ ] 停止后输入法从列表移除，CLSID 注册被清除。

## UAC 提权
- [ ] 非管理员运行 `-start` → 弹 UAC；取消 → 报"请求管理员权限失败或被取消"，不崩溃。
- [ ] 同意 UAC → 子进程 `-elevate-register` 完成注册。

## 开机自启
- [ ] 启动后 `HKCU\...\Run\WindInput`（debug 为 `WindInputDev`）= `"<服务exe>"`。
- [ ] 停止后该键被删除。

## 冲突检测
- [ ] 存在安装版（NSIS 注册）时，便携启动器禁用并提示冲突位置。
- [ ] 当前目录有 `uninstall.exe` → 识别为安装版目录，禁用。
- [ ] 另一便携实例正在运行（占据管道）→ 启动提示"先停止该实例"。
- [ ] 另一便携实例已退出但注册残留 → 可安全接管。

## 进程管理
- [ ] `-stop` 能按全路径精确终止本目录的服务，不误杀其他目录同名实例。
- [ ] 占据管道的他处残留实例（不同目录）能被按名兜底终止。

## GUI + 托盘
- [ ] 窗口显示状态/详情/目录三行，启动后 ~1s 内自动变为「运行中」（active 轮询窗口生效）。
- [ ] 「启动服务」期间按钮置灰、显示"正在启动服务..."，完成后按钮态随状态刷新。
- [ ] 启动失败（如取消 UAC）→ 详情行显示错误信息。
- [ ] 托盘菜单：显示窗口 / 隐藏到托盘 / 启动 / 停止 / 设置 / 数据目录 / 退出 全部生效。
- [ ] 「隐藏到托盘」后窗口消失、服务继续运行、托盘常驻；左键/双击托盘或「显示窗口」唤回。
- [ ] 「退出」仅关闭启动器，IME 服务继续运行（符合输入法直觉）。
- [ ] 再次启动 `wind_portable.exe` → 不开新窗口，前置已有窗口（单实例）。
- [ ] 空闲（无操作 15s 后）CPU 占用回落到 ~0（停帧）。

## 部署与在线更新
- [ ] 「更新」→ 选 ZIP（缺 wind_input.exe/wind_tsf.dll 的包应被拒绝并提示）→ 确认 → 停服、替换文件、重启，状态回到「运行中」。
- [ ] 更新包内含新 `wind_portable.exe` 时：自身被改名 `*.old_*`，详情提示「启动器自身已更新，请关闭后重新打开」。
- [ ] 「复制部署」→ 选目标目录（系统保护目录应被拒绝）→ 确认 → 复制成功，目标含全部文件但**不含 userdata/**。
- [ ] 「ZIP 部署」→ 选 ZIP→校验→选目标→确认 → 解压成功。
- [ ] 部署/更新替换产生的 `*.old_*`/`*.bak` 在下次启动时被自动清理。
- [ ] 部署后到目标目录运行 `wind_portable.exe` 可正常以便携模式启动。

## 已知限制（本期）
- 窗口标题栏 X 关闭会退出启动器（windui 无法 veto 关闭）；驻留托盘请用「隐藏到托盘」。
- 托盘菜单项无法置灰（windui 框架层不支持），改为点击时按状态给提示。
- 托盘图标为纯色占位（未接入 .ico）。
