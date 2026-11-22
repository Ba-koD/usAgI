//! 빌드 때 exe에 아이콘·버전 정보를 넣습니다.
//! 아이콘은 앱과 같은 토끼 실루엣(usagi-core)으로 여러 크기를 그려 .ico로 묶습니다.

use std::path::PathBuf;

const SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../crates/usagi-core/src/rabbit.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    // ---- 아이콘 (.ico 안에 PNG로)
    let images: Vec<(u32, Vec<u8>)> = SIZES
        .iter()
        .map(|&n| {
            let pm = usagi_core::rabbit::app_icon(n).expect("icon");
            (n, pm.encode_png().expect("png"))
        })
        .collect();
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (n, png) in &images {
        let side = if *n >= 256 { 0 } else { *n as u8 };
        ico.extend_from_slice(&[side, side, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in &images {
        ico.extend_from_slice(png);
    }
    let ico_path = out.join("usagi.ico");
    std::fs::write(&ico_path, &ico).expect("write ico");

    // ---- 리소스: 아이콘(ID 1) + 버전 정보
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into());
    let nums: Vec<u16> = ver.split('.').map(|p| p.parse().unwrap_or(0)).chain(std::iter::repeat(0)).take(4).collect();
    let comma = nums.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(",");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{ico}"
1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "041204B0"
    BEGIN
      VALUE "CompanyName", "usAgI"
      VALUE "FileDescription", "usAgI - AI 구독 사용량 표시"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "usagi"
      VALUE "OriginalFilename", "usagi.exe"
      VALUE "ProductName", "usAgI"
      VALUE "ProductVersion", "{ver}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x412, 1200
  END
END
"#,
        ico = ico_path.to_string_lossy().replace('\\', "\\\\"),
    );
    let rc_path = out.join("usagi.rc");
    std::fs::write(&rc_path, rc).expect("write rc");
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_optional().expect("resource");
}
