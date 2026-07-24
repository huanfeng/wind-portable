// 为 wind_portable.exe 嵌入 Windows 资源：图标 / 版本信息 / 应用清单(manifest)。
//
// 用 winresource（与 wind_input 同款）而非 embed-resource：后者在原生 windows CI 上
// rc.exe 编译失败，会导致图标 + manifest 一起丢失，而 manifest 缺失是**致命**的 ——
// windui 静态导入 comctl32 v6 的 TaskDialogIndirect，无 Common-Controls 6.0 manifest 时
// 加载器绑到 comctl32 v5（不导出该函数）→「无法定位程序输入点 TaskDialogIndirect」→
// exe 根本无法启动。winresource 在同一 CI 环境下(wind_input)已验证可正常嵌入。
//
// build.rs 运行在 host，按**目标**平台判断（CARGO_CFG_TARGET_OS）；仅 Windows 目标生效。

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    // 版本优先级: WIND_APP_VERSION(WindInput 打包脚本/CI 从 docs/VERSION 注入)
    //           > {manifest}/../docs/VERSION 文件 > CARGO_PKG_VERSION。
    // 注: 本仓是独立 sibling, {manifest}/../docs/VERSION 通常不指向 WindInput/docs/VERSION
    //     (层级不符, 一般落空), 故主要依赖 WIND_APP_VERSION 注入, 与主仓 / 安装包统一。
    println!("cargo:rerun-if-env-changed=WIND_APP_VERSION");
    let version_file = format!("{manifest_dir}/../docs/VERSION");
    let ver = std::env::var("WIND_APP_VERSION")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string(&version_file)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap()); // 如 0.0.1-alpha

    // 数值版本取 a.b.c 前缀（去掉 -alpha / -dev.xxxx 等后缀）。
    let core = ver.split(['-', '+']).next().unwrap_or("0.0.0");
    let mut parts = core.split('.').map(|s| s.parse::<u16>().unwrap_or(0));
    let maj = parts.next().unwrap_or(0);
    let min = parts.next().unwrap_or(0);
    let pat = parts.next().unwrap_or(0);
    let ver_u64 = ((maj as u64) << 48) | ((min as u64) << 32) | ((pat as u64) << 16);

    let ico = format!("{manifest_dir}/res/app.ico");

    // 应用清单：只声明 Common-Controls 6.0.0.0 → 启用 TaskDialog 等 v6 API + 系统对话框
    // 视觉样式。**故意不声明 dpiAware**：windui 运行时调 SetProcessDpiAwarenessContext
    // (PER_MONITOR_V2)，manifest 里的 DPI 声明优先级更高，一旦写上会锁死并使 windui 的
    // 运行时设置失效。这里只启用视觉样式 / v6 控件，不碰 DPI。
    let manifest_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency><dependentAssembly>
    <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
  </dependentAssembly></dependency>
</assembly>
"#;

    println!("cargo:rerun-if-changed=res/app.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={version_file}");

    let mut res = winresource::WindowsResource::new();
    // ★ 图标资源 ID 必须为 "1"：windui 窗口类用 LoadIcon(hinst, MAKEINTRESOURCE(1)) 取它。
    //   用 set_icon_with_id 显式钉死，勿依赖 set_icon 的默认 ID。
    res.set_icon_with_id(&ico, "1");
    res.set_manifest(manifest_xml);
    res.set_language(0x0804); // 简体中文：版本信息语言块
    res.set("CompanyName", "清风输入法");
    res.set("FileDescription", "清风输入法启动器");
    res.set("FileVersion", &ver);
    res.set("InternalName", "wind_portable");
    res.set("LegalCopyright", "Copyright © 清风输入法");
    res.set("OriginalFilename", "wind_portable.exe");
    res.set("ProductName", "清风输入法");
    res.set("ProductVersion", &ver);
    res.set_version_info(winresource::VersionInfo::FILEVERSION, ver_u64);
    res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, ver_u64);
    if let Err(e) = res.compile() {
        // 注意：manifest 缺失会致 exe 无法启动（缺 TaskDialogIndirect 入口），
        // 不能当外观问题轻视 —— 发布链应把此警告视为构建失败对待。
        println!("cargo:warning=app 资源嵌入失败: {e}");
    }
}
