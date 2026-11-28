//! usAgI for Windows — 작업 표시줄 위젯 + 트레이 토끼.
//!
//!   usagi.exe                               앱 실행 (이미 떠 있으면 설정 창을 엶)
//!   usagi.exe --autostart                   로그인할 때 자동 실행 (이미 떠 있으면 조용히 끝냄)
//!   usagi.exe --installed <pid>             설치 위치로 옮긴 사본의 첫 실행 (예전 프로세스를 기다린 뒤 설정 창)
//!   usagi.exe statusline --account <id>     Claude Code 상태 표시줄 브리지 (stdin JSON → 사용량 기록)
//!   usagi.exe --status                      현재 상태를 출력 (확인용)
//!   usagi.exe --monitors                    모니터 목록 출력 (확인용)

#![windows_subsystem = "windows"]

mod app;
mod autostart;
mod install;
mod settings;
mod toast;
mod tray;
mod uia;
mod updater;
mod widget;
mod win;

use std::io::Write;
use std::process::Command;
use usagi_core::{bridge, config, engine, texts};

fn main() {
    // 가장 먼저: 실행 중에 읽는 DLL은 System32에서만 찾음. 이 exe의 폴더(내려받기 등)와 작업 폴더는 찾지 않음.
    // exe가 가져다 쓰는 DLL은 링크 설정(/DEPENDENTLOADFLAG, build.rs)이 같은 규칙을 적용
    unsafe {
        use windows_sys::Win32::System::LibraryLoader::{SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32};
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == uia::HELPER_ARG) {
        if uia::run_helper(&args[2..]).is_err() {
            std::process::exit(1);
        }
        return;
    }
    install_panic_log();
    match args.get(1).map(String::as_str) {
        Some("--settings-ui") => settings::run_standalone(),
        Some("statusline") => bridge::statusline_main(&args, run_chain),
        Some("--widget-preview") => widget_preview(args.get(2).map(String::as_str).unwrap_or("widget-preview.bmp")),
        // 디자인 확인용: 설정 창 미리 보기에 보내는 것과 같은 자료를 예시 값으로 출력
        Some("--widget-preview-json") => widget_preview_json(&args[2..]),
        Some("--test-notify") => {
            // 알림 확인용: 설정의 "방해 금지 중에도 알림"을 따라 한 번 보냄
            toast::refresh_icon();
            unsafe { windows_sys::Win32::System::Ole::OleInitialize(std::ptr::null_mut()) };
            let urgent = usagi_core::config::State::load().pref_bool("urgent", true);
            match toast::show("사용량 알림", "알림 미리 보기 · Claude 5시간 한도의 80% 사용.", urgent, (std::ptr::null_mut(), 0)) {
                Ok(()) => println!("알림 전송 완료 (중요 알림: {})", urgent && toast::urgent_supported()),
                Err(e) => println!("알림 전송 실패: {e}"),
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Some("--monitors") => {
            for (i, m) in win::monitors().iter().enumerate() {
                println!(
                    "모니터 {} · {} · {}×{}{}  [{}]",
                    i + 1,
                    m.name,
                    m.rect.right - m.rect.left,
                    m.rect.bottom - m.rect.top,
                    if m.primary { " (주 모니터)" } else { "" },
                    m.device
                );
            }
        }
        Some("--status") => {
            let snap = engine::snapshot_once();
            println!("{}", texts::tooltip(Some(&snap)));
            for line in texts::menu_lines(Some(&snap)) {
                println!("{line}");
            }
        }
        Some("--after-update") => {
            // 업데이트로 다시 시작: 예전 프로세스가 끝나길 기다린 뒤 평소처럼 실행
            if let Some(pid) = args.get(2).and_then(|p| p.parse().ok()) {
                updater::wait_for(pid);
            }
            app::run(app::Start::Normal);
        }
        Some(install::ARG) => {
            // 설치 위치로 옮긴 사본: 예전 프로세스가 끝나길 기다린 뒤 설정 창을 열어 이어서 설정
            if let Some(pid) = args.get(2).and_then(|p| p.parse().ok()) {
                updater::wait_for(pid);
            }
            app::run(app::Start::Installed);
        }
        Some(autostart::ARG) => app::run(app::Start::Autostart),
        Some("--check-update") => match updater::check() {
            Ok(Some(r)) => println!("새 버전 v{} (지금 v{}) · {}", r.version, usagi_core::VERSION, r.page),
            Ok(None) => println!("최신 버전 (v{}) · {}", usagi_core::VERSION, updater::REPO),
            Err(e) => println!("확인 실패: {e} · {}", updater::REPO),
        },
        #[cfg(debug_assertions)]
        Some("--sha256") => {
            // 개발용: 업데이트 검증과 같은 방식으로 파일 해시
            let data = std::fs::read(args.get(2).map(String::as_str).unwrap_or("")).unwrap_or_default();
            println!("{}", usagi_core::sha256::hex(&data));
        }
        #[cfg(debug_assertions)]
        Some("--apply-staged") => {
            // 개발용: exe 옆 usagi.exe.new 로 바꾸고 다시 시작
            let staged = std::env::current_exe().map(|e| e.with_extension("exe.new")).unwrap_or_default();
            match updater::apply(&staged) {
                Ok(()) => println!("적용 완료. 새 버전 시작"),
                Err(e) => println!("{e}"),
            }
        }
        _ => app::run(app::Start::Normal),
    }
}

/// 원래 쓰던 상태 표시줄 실행 (`statusline` 명령 본문은 코어의 [`bridge::statusline_main`])
fn run_chain(cmd: &str, input: &str) -> Option<String> {
    // Claude Code는 Windows에서 Git Bash로 상태 표시줄을 실행하므로 bash 우선, 없으면 cmd.
    // 셸은 절대 경로로만 (이름만 주면 Windows가 이 exe의 폴더를 먼저 찾음)
    let system = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| r"C:\Windows".into()).join("System32");
    let attempts: [(Option<std::path::PathBuf>, &[&str]); 2] = [(git_bash(&system), &["-c"]), (Some(system.join("cmd.exe")), &["/C"])];
    for (shell, flag) in attempts {
        let Some(shell) = shell else { continue };
        let mut command = Command::new(shell);
        command.args(flag).arg(cmd);
        if let Some(out) = usagi_core::login::run_statusline_chain(command, input) {
            return Some(out);
        }
        return None;
    }
    None
}

/// Git Bash 찾기: Claude Code가 쓰는 설정(CLAUDE_CODE_GIT_BASH_PATH) → Git 기본 설치 위치 → PATH.
/// System32의 bash.exe(WSL)와 이 exe 폴더는 건너뜀
fn git_bash(system: &std::path::Path) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let own = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from));
    let same = |a: &std::path::Path, b: &std::path::Path| a.to_string_lossy().trim_end_matches('\\').eq_ignore_ascii_case(b.to_string_lossy().trim_end_matches('\\'));
    let fixed = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from).into_iter().chain(
        ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"].into_iter().filter_map(|v| std::env::var_os(v)).map(|base| {
            let base = PathBuf::from(base);
            if base.to_string_lossy().to_lowercase().ends_with("local") { base.join(r"Programs\Git\bin\bash.exe") } else { base.join(r"Git\bin\bash.exe") }
        }),
    );
    let in_path = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).filter(|dir| {
        dir.is_absolute() && !same(dir, system) && !own.as_deref().is_some_and(|o| same(dir, o))
    }).map(|dir| dir.join("bash.exe"));
    fixed.chain(in_path).find(|p| p.is_absolute() && p.is_file())
}

fn install_panic_log() {
    std::panic::set_hook(Box::new(|info| {
        let dir = config::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("usagi.log")) {
            let _ = writeln!(f, "[{}] panic: {info}", usagi_core::time::now() as i64);
        }
    }));
}

/// 디자인 확인용: 예시 값으로 그린 위젯 미리 보기 자료(JSON) 출력. 인자: nobars · time · light · spent · bar=<px>
fn widget_preview_json(flags: &[String]) {
    use usagi_core::engine::ProviderId;
    use widget::{Content, Row, Section};
    let has = |f: &str| flags.iter().any(|a| a == f);
    let (time, light, spent) = (has("time"), has("light"), has("spent"));
    let bar_px = flags.iter().find_map(|a| a.strip_prefix("bar=")).and_then(|v| v.parse::<f32>().ok());
    let row = |key: &str, label: &str, ratio: f64, remaining: f64| Row {
        key: key.into(),
        label: label.into(),
        value: if time { usagi_core::texts::fmt_hm(remaining) } else { usagi_core::texts::fmt_usage(ratio, false) },
        ratio,
        left: false,
        percent: !time,
        short: None,
    };
    let content = Content {
        time_mode: time,
        sections: vec![
            if has("loggedout") {
                // 로그아웃된 계정: 그래프 대신 안내 글자
                Section { id: ProviderId::Claude, key: "claude-1".into(), rows: vec![], note: Some(usagi_core::texts::login_needed().into()) }
            } else {
                Section {
                    id: ProviderId::Claude,
                    key: "claude-1".into(),
                    rows: vec![row("5시간", "5시간", if spent { 1.0 } else { 0.62 }, 8000.0), row("주간", "주간", 0.38, 410000.0), row("Fable 주간", "Fable", 0.03, 560000.0)],
                    note: None,
                }
            },
            Section { id: ProviderId::Codex, key: "codex-1".into(), rows: vec![row("5시간", "5시간", 0.47, 8000.0), row("주간", "주간", 0.12, 500000.0)], note: None },
        ],
        rabbit: None,
        ink: if light { win::INK_ON_LIGHT } else { win::INK_ON_DARK },
    };
    println!("{}", widget::preview_json(&content, !has("nobars"), bar_px, 1.0, light));
}

/// 디자인 확인용: 위젯을 어두운/밝은 작업 표시줄 배경에 그려 BMP로 저장
fn widget_preview(path: &str) {
    use usagi_core::engine::ProviderId;
    use widget::{Content, Row, Section};
    let row = |label: &str, ratio: f64| Row { key: label.into(), label: label.into(), value: usagi_core::texts::fmt_pct(ratio), ratio, left: false, percent: true, short: None };
    let mut tiles = Vec::new();
    for (bg, ink) in [((32u8, 32u8, 32u8), win::INK_ON_DARK), ((238, 238, 238), win::INK_ON_LIGHT)] {
        for scale in [1.0f32, 1.5] {
            let content = Content {
                time_mode: false,
                sections: vec![
                    Section { id: ProviderId::Claude, key: "claude-1".into(), rows: vec![row("5시간", 0.15), row("주간", 0.58), row("Fable", 0.03)], note: None },
                    Section { id: ProviderId::Codex, key: "codex-1".into(), rows: vec![row("주간", 0.07)], note: None },
                ],
                // 실제 위젯과 같게 (위젯에는 토끼를 그리지 않음. 토끼는 알림 영역 아이콘)
                rabbit: None,
                ink,
            };
            for level in [0u8, 1, 2] {
                let img = widget::preview(&content, scale, (48.0 * scale) as i32, level);
                tiles.push((img, bg));
            }
        }
    }
    // 퍼센트 → 남은 시간 전환 장면들 (어두운 배경, 150%)
    {
        let rows = |v: [&str; 2]| vec![
            Row { key: "5시간".into(), label: "5시간".into(), value: v[0].into(), ratio: 0.15, left: false, percent: false, short: Some("2h13m".into()) },
            Row { key: "주간".into(), label: "주간".into(), value: v[1].into(), ratio: 0.58, left: false, percent: false, short: Some("6d23h".into()) },
        ];
        let content = Content {
            time_mode: true,
            sections: vec![Section { id: ProviderId::Claude, key: "claude-1".into(), rows: rows(["2시간 13분", "6일 23시간"]), note: None }],
            rabbit: None,
            ink: win::INK_ON_DARK,
        };
        let from: std::collections::HashMap<(usize, String), String> =
            [((0, "5시간".to_string()), "15%".to_string()), ((0, "주간".to_string()), "58%".to_string())].into_iter().collect();
        // 남은 시간 + 아주 좁은 모양 (짧은 값)
        for level in [1u8, 2] {
            tiles.push((widget::preview(&content, 1.5, 72, level), (32, 32, 32)));
        }
        for t in [0.0f32, 0.15, 0.3, 0.45, 0.6, 0.8, 1.0] {
            let img = widget::preview_switch(&content, 1.5, 72, 0, Some((&from, t)));
            tiles.push((img, (32, 32, 32)));
        }
    }
    // 막대 길이 설정: 가장 짧게(0) · 기본 · 중간 · 최대, 각각 남은 시간 / 퍼센트 (어두운 배경, 100%)
    {
        let time = |v: &str, ratio: f64| Row { key: "5시간".into(), label: "5시간".into(), value: v.into(), ratio, left: false, percent: false, short: None };
        for bar in [None, Some(0.0f32), Some(160.0), Some(240.0)] {
            for time_mode in [true, false] {
                let rows = if time_mode {
                    vec![time("23시간 59분", 0.15), Row { key: "주간".into(), label: "주간".into(), ..time("6일 23시간", 0.58) }]
                } else {
                    vec![row("5시간", 0.15), row("주간", 0.58)]
                };
                let content = Content {
                    time_mode,
                    sections: vec![Section { id: ProviderId::Claude, key: "claude-1".into(), rows, note: None }],
                    rabbit: None,
                    ink: win::INK_ON_DARK,
                };
                tiles.push((widget::preview_bar(&content, 1.0, 48, 0, None, bar), (32, 32, 32)));
            }
        }
    }
    let w = tiles.iter().map(|(i, _)| i.w).max().unwrap_or(1);
    let h: u32 = tiles.iter().map(|(i, _)| i.h).sum();
    let mut px = vec![0u8; (w * h * 4) as usize];
    let mut y0 = 0;
    for (img, (r, g, b)) in &tiles {
        for y in 0..img.h {
            for x in 0..w {
                let o = (((y0 + y) * w + x) * 4) as usize;
                let (mut pb, mut pg, mut pr) = (*b as u32, *g as u32, *r as u32);
                if x < img.w {
                    let i = ((y * img.w + x) * 4) as usize;
                    let a = img.px[i + 3] as u32;
                    pb = img.px[i] as u32 + pb * (255 - a) / 255;
                    pg = img.px[i + 1] as u32 + pg * (255 - a) / 255;
                    pr = img.px[i + 2] as u32 + pr * (255 - a) / 255;
                }
                px[o..o + 4].copy_from_slice(&[pb.min(255) as u8, pg.min(255) as u8, pr.min(255) as u8, 255]);
            }
        }
        y0 += img.h;
    }
    let mut out = Vec::new();
    let size = 54 + px.len() as u32;
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    out.extend_from_slice(&px);
    let _ = std::fs::write(path, out);
}
