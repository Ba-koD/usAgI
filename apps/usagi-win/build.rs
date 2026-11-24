//! 빌드 때 exe에 아이콘·버전 정보를 넣습니다.
//! 아이콘은 앱과 같은 토끼 실루엣(usagi-core)으로 여러 크기를 그려 .ico로 묶습니다.

use std::path::PathBuf;

const SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

/// 앱 매니페스트. supportedOS(Windows 8 · 8.1 · 10/11)가 있어야 WS_EX_LAYERED 자식 창을 만들 수 있음
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
      <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
      <supportedOS Id="{4a2f28e3-53b9-4441-ba9c-d69d4a4a6e38}"/>
    </application>
  </compatibility>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;

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
    // ---- 매니페스트: Windows 8 이상 호환 선언 (투명 위젯을 작업 표시줄의 자식 창으로 붙이려면 필요), 관리자 권한 요청 없음
    let manifest_path = out.join("usagi.manifest");
    std::fs::write(&manifest_path, MANIFEST).expect("write manifest");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{ico}"
1 24 "{manifest}"
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
        manifest = manifest_path.to_string_lossy().replace('\\', "\\\\"),
    );
    let rc_path = out.join("usagi.rc");
    std::fs::write(&rc_path, rc).expect("write rc");
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_optional().expect("resource");
}
