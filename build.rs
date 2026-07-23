use std::io::Write;

fn main() {
    // build.rs 运行在 host(Linux)，按**目标**平台判断（CARGO_CFG_TARGET_OS）。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out = std::env::var("OUT_DIR").unwrap();
    // 版本优先级: WIND_APP_VERSION (WindInput 打包脚本/CI 从 docs/VERSION 注入) > docs/VERSION 文件 > CARGO_PKG_VERSION。
    // 注: 本仓是独立 sibling, {manifest}/../docs/VERSION 并不指向 WindInput/docs/VERSION（层级不符, 通常落空）,
    //     故主要依赖 WIND_APP_VERSION 注入, 与 wind_input / wind_tsf / wind-setting / 安装包统一。
    println!("cargo:rerun-if-env-changed=WIND_APP_VERSION");
    let version_file = format!("{manifest}/../docs/VERSION");
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
                                                                         // 数字 FILEVERSION 取版本号的 a.b.c 前缀（去掉 -alpha/-dev.xxxx 等后缀）。
    let core = ver.split(['-', '+']).next().unwrap_or("0.0.0");
    let mut parts = core.split('.').map(|s| s.parse::<u32>().unwrap_or(0));
    let maj = parts.next().unwrap_or(0);
    let min = parts.next().unwrap_or(0);
    let pat = parts.next().unwrap_or(0);
    // ICON 资源 ID 1（windui 窗口类 LoadIcon(hinst, MAKEINTRESOURCE(1)) 取用）+ VERSIONINFO。
    // 用 UTF-8（无 BOM）+ #pragma code_page(65001)，使 RC 编译器按 UTF-8 解析中文字符串
    //（embed-resource 先用 clang-cl 预处理，不支持 UTF-16 BOM）。
    let ico = format!("{manifest}/res/app.ico"); // 正斜杠，避免 .rc 转义

    // 应用清单：声明 Common-Controls 6.0.0.0 → 系统对话框（MessageBox / TaskDialog）启用
    // 视觉样式，按钮从经典方角变为现代主题化外观。**故意不声明 dpiAware**：windui 运行时
    // 调 SetProcessDpiAwarenessContext(PER_MONITOR_V2)，manifest 里的 DPI 声明优先级更高，
    // 一旦写上会锁死并使 windui 的运行时设置失败。这里只启用视觉样式，不碰 DPI。
    let manifest_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
        <assembly xmlns=\"urn:schemas-microsoft-com:asm.v1\" manifestVersion=\"1.0\">\r\n\
        \x20 <dependency><dependentAssembly>\r\n\
        \x20   <assemblyIdentity type=\"win32\" name=\"Microsoft.Windows.Common-Controls\" \
        version=\"6.0.0.0\" processorArchitecture=\"*\" publicKeyToken=\"6595b64144ccf1df\" \
        language=\"*\"/>\r\n\
        \x20 </dependentAssembly></dependency>\r\n\
        </assembly>\r\n";
    let mft_path = format!("{out}/wind_portable.manifest");
    std::fs::File::create(&mft_path)
        .unwrap()
        .write_all(manifest_xml.as_bytes())
        .unwrap();
    // RT_MANIFEST(24) id 1 = CREATEPROCESS_MANIFEST_RESOURCE_ID（exe 主清单）。
    // 正斜杠：本机 Windows 构建时 OUT_DIR 为反斜杠，进 .rc 字符串会被当转义序列。
    let mft_rc = mft_path.replace('\\', "/");

    let rc = format!(
        "#pragma code_page(65001)\n\
         1 ICON \"{ico}\"\n\
         1 24 \"{mft_rc}\"\n\
         \n\
         1 VERSIONINFO\n\
         FILEVERSION {maj},{min},{pat},0\n\
         PRODUCTVERSION {maj},{min},{pat},0\n\
         FILEOS 0x40004L\n\
         FILETYPE 0x1L\n\
         BEGIN\n\
         \x20 BLOCK \"StringFileInfo\"\n\
         \x20 BEGIN\n\
         \x20   BLOCK \"080404B0\"\n\
         \x20   BEGIN\n\
         \x20     VALUE \"CompanyName\", \"清风输入法\"\n\
         \x20     VALUE \"FileDescription\", \"清风输入法启动器\"\n\
         \x20     VALUE \"FileVersion\", \"{ver}\"\n\
         \x20     VALUE \"InternalName\", \"wind_portable\"\n\
         \x20     VALUE \"LegalCopyright\", \"Copyright © 清风输入法\"\n\
         \x20     VALUE \"OriginalFilename\", \"wind_portable.exe\"\n\
         \x20     VALUE \"ProductName\", \"清风输入法\"\n\
         \x20     VALUE \"ProductVersion\", \"{ver}\"\n\
         \x20   END\n\
         \x20 END\n\
         \x20 BLOCK \"VarFileInfo\"\n\
         \x20 BEGIN\n\
         \x20   VALUE \"Translation\", 0x804, 1200\n\
         \x20 END\n\
         END\n"
    );
    let rc_path = format!("{out}/wind_portable.rc");
    std::fs::File::create(&rc_path)
        .unwrap()
        .write_all(rc.as_bytes())
        .unwrap();
    println!("cargo:rerun-if-changed=res/app.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={version_file}");

    let r = embed_resource::compile(&rc_path, embed_resource::NONE);
    if let Err(e) = r.manifest_optional() {
        println!("cargo:warning=app 资源嵌入失败: {e}");
    }
}
