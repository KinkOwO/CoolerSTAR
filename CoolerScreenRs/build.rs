//! Windows 资源：应用图标 + 版本信息
//!
//! 用 windres 把 `.rc` 编成 COFF 目标文件再交给链接器，不引 winres 之类的 crate ——
//! 这个项目只编 Windows，mingw 工具链里本来就有 windres。
//!
//! 版本号从 `CARGO_PKG_VERSION` 取，改 `Cargo.toml` 就够了，这里不用手工同步。
//!
//! **为什么预处理要拐个弯**：这台机器上的 mingw 是精简版，只有 `windres.exe`，
//! 没有 `gcc.exe`。windres 默认拿 `gcc -E` 做预处理，找不到就直接
//! `preprocessing failed`。所以这里显式把预处理器换成 `cmd /c type` ——
//! 也就是"原样吐出文件内容"，反正我们的 rc 里一条 `#include`、一条 `#define`
//! 都没有，根本不需要真正的预处理。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

    // RC 的 FILEVERSION 要四段纯数字：1.0.0 -> 1,0,0,0
    let mut parts: Vec<u32> = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u32>().ok())
        .collect();
    while parts.len() < 4 {
        parts.push(0);
    }
    parts.truncate(4);
    let numeric = format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]);

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let rc_path = out_dir.join("app.rc");
    let res_path = out_dir.join("app_res.o");
    let ico_path = out_dir.join("app_icon.ico");

    // 图标也复制到 OUT_DIR：rc 里就能写相对名，不会碰到中文路径和空格
    let src_ico = Path::new("assets/icon.ico");
    if src_ico.exists() {
        let _ = std::fs::copy(src_ico, &ico_path);
    }

    let icon_line = if ico_path.exists() {
        "1 ICON \"app_icon.ico\"\n\n"
    } else {
        ""
    };

    let rc = format!(
        "{icon_line}1 VERSIONINFO\n\
         FILEVERSION {numeric}\n\
         PRODUCTVERSION {numeric}\n\
         FILEFLAGSMASK 0x3fL\n\
         FILEFLAGS 0x0L\n\
         FILEOS 0x40004L\n\
         FILETYPE 0x1L\n\
         FILESUBTYPE 0x0L\n\
         BEGIN\n\
         \x20   BLOCK \"StringFileInfo\"\n\
         \x20   BEGIN\n\
         \x20       BLOCK \"080404b0\"\n\
         \x20       BEGIN\n\
         \x20           VALUE \"CompanyName\",      \"KinkOwO\"\n\
         \x20           VALUE \"FileDescription\",  \"CoolerSTAR - Boomstar water-cooler console\"\n\
         \x20           VALUE \"FileVersion\",      \"{version}\"\n\
         \x20           VALUE \"InternalName\",     \"CoolerSTAR\"\n\
         \x20           VALUE \"LegalCopyright\",   \"MIT License\"\n\
         \x20           VALUE \"OriginalFilename\", \"CoolerSTAR.exe\"\n\
         \x20           VALUE \"ProductName\",      \"CoolerSTAR\"\n\
         \x20           VALUE \"ProductVersion\",   \"{version}\"\n\
         \x20       END\n\
         \x20   END\n\
         \x20   BLOCK \"VarFileInfo\"\n\
         \x20   BEGIN\n\
         \x20       VALUE \"Translation\", 0x804, 1200\n\
         \x20   END\n\
         END\n"
    );

    std::fs::write(&rc_path, rc).expect("写 app.rc 失败");

    if !run_windres(&out_dir, &rc_path, &res_path) {
        println!("cargo:warning=windres 没能编出资源文件，这次构建不带图标和版本信息");
        return;
    }
    println!("cargo:rustc-link-arg-bins={}", res_path.display());
}

/// 先试 PATH 上的 windres，再试 rustup 装的那份。
fn run_windres(out_dir: &Path, rc_path: &Path, res_path: &Path) -> bool {
    let candidates: Vec<PathBuf> = std::env::var("WINDRES")
        .map(PathBuf::from)
        .into_iter()
        .chain(std::iter::once(PathBuf::from("windres")))
        .chain(std::env::var("USERPROFILE").ok().map(|h| {
            PathBuf::from(h)
                .join(".rustup")
                .join("mingw-bin")
                .join("mingw64")
                .join("bin")
                .join("windres.exe")
        }))
        .collect();

    for exe in candidates {
        let status = Command::new(&exe)
            .current_dir(out_dir)
            // 没有 gcc，用 cmd 的 type 当"预处理器"：原样吐内容
            .arg("--preprocessor=C:\\Windows\\System32\\cmd.exe")
            .arg("--preprocessor-arg=/c")
            .arg("--preprocessor-arg=type")
            .arg("-J")
            .arg("rc")
            .arg("-O")
            .arg("coff")
            .arg(rc_path.file_name().unwrap_or(rc_path.as_os_str()))
            .arg(res_path.file_name().unwrap_or(res_path.as_os_str()))
            .status();
        if matches!(&status, Ok(s) if s.success()) && res_path.exists() {
            return true;
        }
    }
    false
}
