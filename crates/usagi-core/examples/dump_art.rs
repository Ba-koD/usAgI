//! 토끼 포즈와 공급자 로고를 PGM(흑백) 파일로 저장합니다 — 디자인 확인용.
//!     cargo run -p usagi-core --example dump_art -- <출력 폴더>
use usagi_core::animator::{Burrow, Pose};
use usagi_core::engine::ProviderId;
use usagi_core::{logos, rabbit};

fn save(dir: &std::path::Path, name: &str, size: u32, alpha: &[u8]) {
    let mut data = format!("P5\n{size} {size}\n255\n").into_bytes();
    data.extend_from_slice(alpha);
    std::fs::write(dir.join(format!("{name}.pgm")), data).unwrap();
}

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&dir).unwrap();
    let poses = [
        ("rest", Pose::gauges(0.1, 0.05)),
        ("hop", Pose { lift: 8, squash: -2, ear_l: 4, ear_r: 4, ..Pose::gauges(0.45, 0.3) }),
        ("half", Pose::gauges(0.6, 0.4)),
        ("tired", Pose { sweat: Some(2), squash: 2, ..Pose::gauges(0.9, 0.5) }),
        ("groom", Pose { paw: Some(1), ..Pose::gauges(0.2, 0.2) }),
        ("nap", Pose { burrow: Burrow::Sleep, zzz: Some(2), ..Pose::gauges(1.0, 0.6) }),
        ("peek", Pose { burrow: Burrow::Peek, ..Default::default() }),
    ];
    for size in [16u32, 32, 64] {
        for (name, p) in &poses {
            save(&dir, &format!("rabbit_{name}_{size}"), size, &rabbit::render(p, size));
        }
        save(&dir, &format!("logo_claude_{size}"), size, &logos::render(ProviderId::Claude, size));
        save(&dir, &format!("logo_codex_{size}"), size, &logos::render(ProviderId::Codex, size));
    }
    println!("saved to {}", dir.display());
}
