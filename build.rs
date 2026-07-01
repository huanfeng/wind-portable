use std::io::Write;

fn main() {
    // build.rs 运行在 host(Linux)，按**目标**平台判断（CARGO_CFG_TARGET_OS）。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out = std::env::var("OUT_DIR").unwrap();
    // 版本号优先取项目 docs/VERSION（发版时 release.yml 把 tag 版本写入它，统一各产物的资源版本）；
    // 缺失则回退 CARGO_PKG_VERSION。
    let version_file = format!("{manifest}/../docs/VERSION");
    let ver = std::fs::read_to_string(&version_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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
    let rc = format!(
        "#pragma code_page(65001)\n\
         1 ICON \"{ico}\"\n\
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
         \x20     VALUE \"CompanyName\", \"WindInput Contributors\"\n\
         \x20     VALUE \"FileDescription\", \"清风输入法便携启动器\"\n\
         \x20     VALUE \"FileVersion\", \"{ver}\"\n\
         \x20     VALUE \"InternalName\", \"wind_portable\"\n\
         \x20     VALUE \"LegalCopyright\", \"Copyright (C) 2026 WindInput Contributors\"\n\
         \x20     VALUE \"OriginalFilename\", \"wind_portable.exe\"\n\
         \x20     VALUE \"ProductName\", \"清风输入法 (WindInput)\"\n\
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
