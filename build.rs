// 构建脚本：把应用清单(manifest)与图标资源直接嵌入到 exe 中。
//
// 这里刻意不使用 winres / embed-resource 等额外构建依赖：manifest 通过 MSVC
// 链接器参数 /MANIFEST:EMBED 嵌入；图标由 build/icon.rs 用纯 Rust 绘制并直接
// 写成 .res 文件交给链接器（无需 rc.exe）。项目的依赖树只有 windows-sys 一个
// crate，符合“体积小巧、零额外依赖”的目标。
//
// manifest 的作用：
//   * 声明 Per-Monitor-V2 DPI 感知，高分屏下界面清晰不模糊
//   * 启用 Common Controls v6 视觉样式
//   * requestedExecutionLevel=asInvoker，不强制管理员权限
//
// 图标：exe 文件图标、窗口与托盘共用同一份资源。生成的 app.ico 也会写到
// OUT_DIR（target/<profile>/build/better-boss-key-*/out/），便于预览。

#[path = "build/icon.rs"]
mod icon;

use std::path::{Path, PathBuf};

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("app.manifest");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=build/icon.rs");

    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env != "msvc" {
        return;
    }

    // manifest
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    // 关闭链接器自动生成的默认 manifest，避免与我们的发生冲突。
    println!("cargo:rustc-link-arg=/MANIFESTUAC:NO");

    // 图标
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let images: Vec<(u32, Vec<u8>)> = icon::SIZES
        .iter()
        .map(|&n| (n, icon::png(n, n, &icon::render(n))))
        .collect();
    let res = out_dir.join("app_icon.res");
    std::fs::write(&res, icon::res(&images, icon::RESOURCE_ID)).expect("写入 app_icon.res");
    std::fs::write(out_dir.join("app.ico"), icon::ico(&images)).expect("写入 app.ico");
    println!("cargo:rustc-link-arg={}", res.display());
}
