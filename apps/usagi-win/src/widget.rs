//! 작업 표시줄 위젯 — 알림 영역(^) 바로 왼쪽.
//!
//!   [토끼] [Claude 로고] 5시간 ▬▬▭▭ 22%  │ [OpenAI 로고] 주간 ▬▭▭▭ 8%
//!                       주간  ▬▭▭▭ 11%
//!
//! - Windows 11에는 deskband가 없으므로 작업 표시줄 위에 겹치는 투명 최상위 창.
//! - 작업 표시줄의 자식/소유 창으로 붙이지 않음: 다른 프로세스(explorer)와 입력 큐가 묶여
//!   한쪽이 멈추면 같이 멈출 수 있음. 대신 1초마다 + 전경 창 변경 때 위치/맨 위를 다시 맞춤.
//! - 기준점: TrayNotifyWnd(알림 영역, ^ 포함)의 왼쪽 끝. 모니터·DPI·작업 표시줄 재생성 모두 따라감.
//! - 자동 숨김으로 내려가 있거나 전체 화면 앱 중이면 숨김. 자리가 모자라면 좁은 모양 → 그래도 없으면 숨김.
//! - 더블클릭: 설정 창, 우클릭: 퍼센트 ↔ 남은 시간. 포커스는 가져가지 않음.

use crate::win::{self, Align, FontSpec, Image, Rgb, TextRenderer};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use tiny_skia::{LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};
use usagi_core::animator::Pose;
use usagi_core::engine::ProviderId;
use usagi_core::{logos, rabbit, texts};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// 논리 크기 (96 DPI 기준 px)
const PAD: f32 = 6.0;
const RABBIT_W: f32 = 30.0;
/// 칸 안에서 막대가 시작하는 위치 (로고·라벨 뒤)
const BAR_X: f32 = 72.0;
/// 막대 길이 설정의 최댓값 (논리 px). 최솟값은 가장 긴 남은 시간 글자 너비
const BAR_MAX: f32 = 240.0;
/// 예전 고정 배치에서 막대 + 값 글자에 쓰던 너비 (칸 207 - 라벨 칸 42 - 간격 11 - 끝 2)
const OLD_BAR_SPAN: f32 = 152.0;

/// 막대 길이 기본값: 예전 고정 배치와 같은 길이 (가장 긴 시간 글자보다는 짧지 않게)
fn default_bar(widest: f32) -> f32 {
    (OLD_BAR_SPAN - widest).max(widest)
}

/// 설정 창에 보일 막대 길이 범위 (논리 px): (최소 = 가장 긴 값 글자, 기본 = 예전 고정 길이, 최대). 언어별로 한 번만 잼
pub fn bar_range() -> (f32, f32, f32) {
    static CACHE: std::sync::Mutex<Option<(&'static str, (f32, f32, f32))>> = std::sync::Mutex::new(None);
    let lang = usagi_core::i18n::lang().code();
    if let Some((l, r)) = *CACHE.lock().unwrap_or_else(|e| e.into_inner()) {
        if l == lang {
            return r;
        }
    }
    let widest = Widget::new(std::ptr::null_mut(), std::ptr::null_mut(), true).widest_value(1.0);
    let range = (widest, default_bar(widest), BAR_MAX.max(widest));
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((lang, range));
    range
}
/// 라벨 글자와 막대 사이 최소 간격 (라벨이 BAR_X보다 길 때만 막대가 뒤로 밀림)
const LABEL_GAP: f32 = 8.0;

/// 값 칸 너비를 정하는 가장 긴 글자들 (남은 시간 "n시간 n분" / "n일 n시간", 퍼센트). 언어마다 글자가 달라 그때 만듦
fn widest_values() -> [String; 3] {
    [texts::fmt_hm(23.0 * 3600.0 + 59.0 * 60.0), texts::fmt_hm(6.0 * 86400.0 + 23.0 * 3600.0), "100%".into()]
}
/// 막대와 값 글자 사이
const VALUE_GAP: f32 = 11.0;
/// 칸 = 로고 + 열(두 줄씩). 세 번째 줄(예: Fable)부터는 같은 칸 안의 다음 열로
const LOGO_COL: f32 = 30.0;
const COL_GAP: f32 = 18.0;
/// 좁은 모양(막대 없음): 로고 → 라벨 → 값. 간격은 넓은 모양과 같은 느낌으로
const COMPACT_LOGO: f32 = 25.0; // 로고 16 + 간격 9
const COMPACT_GAP: f32 = 8.0; // 라벨 → 값
const COMPACT_COL_GAP: f32 = 14.0; // 같은 칸의 열 사이
const COMPACT_DIVIDER: f32 = 15.0; // 칸 사이 (구분선 양쪽 7)

/// 한 칸의 열 수 (두 줄씩)
fn columns(rows: usize) -> usize {
    rows.div_ceil(2).max(1)
}

/// 위젯 모양: 넓음(막대) → 좁음(라벨+값) → 아주 좁음(5h 7d 같은 짧은 라벨+값)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Fit {
    Full,
    Compact,
    Short,
}

/// 아주 좁을 때의 짧은 라벨: 5시간 → 5h, 주간 → 7d, 모델별 주간(Fable 주간) → F7d.
/// 화면 이름이 아니라 저장된 그래프 이름(`Row::key`)으로 고름
fn short_label(label: &str) -> String {
    match label {
        "5시간" => "5h".into(),
        "주간" => "7d".into(),
        other => format!("{}7d", other.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default()),
    }
}

/// 한 칸의 너비 (논리 px). 넓은 모양은 열 너비(col_w), 좁은 모양은 글자 너비(라벨 칸 lab, 값 칸 val)에 맞춤
fn section_width(rows: usize, fit: Fit, lab: f32, val: f32, col_w: f32) -> f32 {
    let c = columns(rows) as f32;
    match fit {
        Fit::Full => LOGO_COL + c * col_w + (c - 1.0) * COL_GAP,
        _ => COMPACT_LOGO + c * (lab + COMPACT_GAP + val) + (c - 1.0) * COMPACT_COL_GAP,
    }
}
const DIVIDER_W: f32 = 19.0;
const MARGIN: f32 = 6.0;

const BAR_COLORS: [Rgb; 3] = [(52, 199, 89), (255, 176, 32), (255, 69, 58)]; // 여유/주의/위험

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Right,
    Left,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// 저장된 그래프 이름 ("5시간", "주간", "Fable 주간"). 막대 값 기억·전환 애니메이션·짧은 라벨의 열쇠라 언어와 무관
    pub key: String,
    /// 화면에 보이는 라벨 (언어에 맞춘 이름)
    pub label: String,
    /// 표시할 값 ("15%", "2시간 13분", "–")
    pub value: String,
    /// 사용 비율 (0..1). 막대 색은 항상 이 값 기준
    pub ratio: f64,
    /// 막대·퍼센트를 남은 양 기준으로
    pub left: bool,
    /// 퍼센트 표시인지 (애니메이션 중 숫자를 다시 계산할지)
    pub percent: bool,
    /// 아주 좁을 때 쓸 짧은 값 (남은 시간 5h23m). 없으면 value 그대로
    pub short: Option<String>,
}

impl Row {
    fn shown(&self, short: bool) -> &str {
        if short {
            self.short.as_deref().unwrap_or(&self.value)
        } else {
            &self.value
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub id: ProviderId,
    /// 계정 id (막대가 이어서 움직이도록 값을 기억하는 열쇠. 순서를 바꿔도 같은 계정이면 그 자리에서 이어짐)
    pub key: String,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Content {
    pub sections: Vec<Section>,
    pub rabbit: Option<Pose>,
    pub ink: Rgb,
    /// 남은 시간 표시 중 (바뀌는 순간 값이 위로 빠지고 새 값이 아래에서 올라옴)
    pub time_mode: bool,
}

/// 퍼센트 ↔ 남은 시간 전환 애니메이션
struct Switch {
    /// 전환 직전에 보이던 값 ((칸 번호, 그래프 key) → 값)
    from: HashMap<(usize, String), String>,
    start: std::time::Instant,
}

const SWITCH_SECS: f32 = 0.36;
/// 막대가 새 값으로 다가가는 시간 상수 (초)
const EASE_TAU: f64 = 0.2;
/// 값 칸 너비가 바뀔 때(퍼센트 ↔ 시간) 막대 길이가 따라가는 시간 상수 (초)
const COL_TAU: f64 = 0.09;

fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Place {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    scale: f32,
    fit: Fit,
}

pub struct Widget {
    pub hwnd: HWND,
    /// 이 위젯이 붙는 작업 표시줄 (주: Shell_TrayWnd, 보조: Shell_SecondaryTrayWnd)
    pub taskbar: HWND,
    pub primary: bool,
    pub enabled: bool,
    pub side: Side,
    shown: bool,
    place: Option<Place>,
    content: Option<Content>,
    last_key: Option<u64>,
    text: TextRenderer,
    layout: Option<(u64, Image)>,
    rabbits: HashMap<(Pose, u32), Vec<u8>>,
    logos: HashMap<(ProviderId, u32), Vec<u8>>,
    /// 화면에 보이는 막대 비율 (목표값으로 부드럽게 다가감)
    eased: HashMap<String, f64>,
    easing: bool,
    last_tick: Option<std::time::Instant>,
    switch: Option<Switch>,
    /// 값 글자 칸 너비 (논리 px). 모든 줄이 같은 값을 써서 막대 길이가 전부 같음
    value_col: Option<f64>,
    /// 라벨 칸 너비 (보통 라벨, 짧은 라벨) — 좁은 모양 너비 계산용
    label_cols: (f32, f32),
    /// 아주 좁은 모양의 값 칸 너비 (짧은 값 기준)
    short_col: f32,
    /// 막대 길이 설정 (0..1, 0 = 가장 긴 남은 시간 글자만큼)
    pub bar_px: Option<f32>,
    /// 넓은 모양의 한 열 너비 (논리 px) = 라벨 + 막대 + 간격 + 가장 긴 값. 모드를 바꿔도 위젯 너비는 그대로
    col_w: f32,
    /// 가장 긴 값 글자 너비 캐시 (배율, 영어인지, 너비)
    widest: (f32, bool, f32),
}

fn hash_of<T: Hash>(v: &T) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

impl Widget {
    pub fn new(hwnd: HWND, taskbar: HWND, primary: bool) -> Self {
        Self {
            hwnd,
            taskbar,
            primary,
            enabled: true,
            side: Side::Right,
            shown: false,
            place: None,
            content: None,
            last_key: None,
            text: TextRenderer::new(),
            layout: None,
            rabbits: HashMap::new(),
            logos: HashMap::new(),
            eased: HashMap::new(),
            easing: false,
            last_tick: None,
            switch: None,
            value_col: None,
            label_cols: (32.0, 18.0),
            short_col: 30.0,
            bar_px: None,
            col_w: 207.0,
            widest: (0.0, false, 0.0),
        }
    }

    /// 가장 긴 값 글자 너비 (논리 px, 배율·언어별로 한 번만 잼)
    fn widest_value(&mut self, scale: f32) -> f32 {
        let en = usagi_core::i18n::is_en();
        if self.widest.0 != scale || self.widest.1 != en {
            let samples = widest_values();
            let w = self.measure_col(samples.iter().map(String::as_str), scale).ceil();
            self.widest = (scale, en, w);
        }
        self.widest.2
    }

    /// 넓은 모양에서 로고 뒤 라벨 칸 (막대 시작까지). 보통 BAR_X, 라벨이 길면(영어 "Weekly" 등) 그만큼 늘어남
    fn label_span(&self) -> f32 {
        (BAR_X - LOGO_COL).max(self.label_cols.0 + LABEL_GAP)
    }

    /// 넓은 모양 한 열 너비. 남은 시간 표시일 때 막대 = 설정 길이(px), 없으면 예전 고정 길이. 가장 긴 시간 글자보다 짧아지지 않음
    fn full_col_width(&mut self, scale: f32) -> f32 {
        let widest = self.widest_value(scale);
        let bar = self.bar_px.unwrap_or_else(|| default_bar(widest)).clamp(widest, BAR_MAX.max(widest));
        self.label_span() + bar + VALUE_GAP + widest + 2.0
    }

    /// 움직이는 중이면 true (그동안 앱이 틱을 촘촘하게)
    pub fn animating(&self) -> bool {
        self.easing || self.switch.is_some()
    }

    /// 전환 진행도 0..1 (전환 중이 아니면 None)
    fn switch_progress(&self) -> Option<f32> {
        self.switch.as_ref().map(|s| (s.start.elapsed().as_secs_f32() / SWITCH_SECS).min(1.0))
    }

    fn width_for(&self, content: &Content, fit: Fit) -> f32 {
        let n = content.sections.len() as f32;
        let val = if fit == Fit::Short { self.short_col } else { self.value_col.unwrap_or(32.0) as f32 };
        let lab = if fit == Fit::Short { self.label_cols.1 } else { self.label_cols.0 };
        let secs: f32 = content.sections.iter().map(|s| section_width(s.rows.len(), fit, lab, val, self.col_w)).sum();
        let rabbit = if content.rabbit.is_some() { RABBIT_W } else { 0.0 };
        let divider = if fit == Fit::Full { DIVIDER_W } else { COMPACT_DIVIDER };
        PAD * 2.0 + rabbit + secs + (n - 1.0).max(0.0) * divider
    }

    /// 라벨 글자 너비 (보통, 짧은) — 논리 px
    fn measure_labels(&mut self, content: &Content, scale: f32) -> (f32, f32) {
        let spec = FontSpec { px: (12.0 * scale).round() as i32, weight: 700, face: "Malgun Gothic" };
        let (mut long, mut short) = (0.0f32, 0.0f32);
        for r in content.sections.iter().flat_map(|s| &s.rows) {
            long = long.max(self.text.measure(&r.label, spec).0 as f32 / scale);
            short = short.max(self.text.measure(&short_label(&r.key), spec).0 as f32 / scale);
        }
        (long.ceil(), short.ceil())
    }

    /// 애니메이션 틱마다. 바뀐 게 있을 때만 다시 그림.
    pub fn update(&mut self, mut content: Content) {
        // 막대·퍼센트가 새 값으로 부드럽게 차오르게 (처음 켤 때는 0에서). 틱 간격과 무관하게 시간 기준.
        let now = std::time::Instant::now();
        let dt = self.last_tick.map(|t| now.duration_since(t).as_secs_f64().min(0.25)).unwrap_or(0.066);
        self.last_tick = Some(now);
        let k = 1.0 - (-dt / EASE_TAU).exp();
        let mut easing = false;
        for sec in content.sections.iter_mut() {
            for row in &mut sec.rows {
                let key = format!("{}:{}", sec.key, row.key);
                let cur = self.eased.get(&key).copied().unwrap_or(0.0);
                let target = row.ratio;
                let next = if (target - cur).abs() < 0.004 { target } else { cur + (target - cur) * k };
                easing |= next != target;
                self.eased.insert(key, next);
                if row.percent && target <= 1.0 {
                    row.value = texts::fmt_pct(if row.left { (1.0 - next).max(0.0) } else { next });
                }
                row.ratio = next;
            }
        }
        // 값 칸 = 지금 보이는 값 중 가장 넓은 것. 막대는 그만큼만 줄고 늘며, 모든 줄이 같은 길이.
        let scale = self.place.map(|p| p.scale).unwrap_or(1.0);
        let target = self.measure_col(content.sections.iter().flat_map(|s| s.rows.iter().map(|r| r.value.as_str())), scale) as f64;
        let col = match self.value_col {
            Some(c) if (target - c).abs() >= 0.3 => c + (target - c) * (1.0 - (-dt / COL_TAU).exp()),
            _ => target,
        };
        easing |= col != target;
        self.value_col = Some(col);
        self.label_cols = self.measure_labels(&content, scale);
        self.short_col = self.measure_col(content.sections.iter().flat_map(|s| s.rows.iter().map(|r| r.shown(true))), scale);
        self.col_w = self.full_col_width(scale);
        // 좁은 모양은 글자 너비에, 넓은 모양은 막대 길이 설정에 맞추므로 너비가 바뀌면 창 크기도 다시
        let resize = self.place.is_some_and(|p| (self.width_for(&content, p.fit) * p.scale).round() as i32 != p.w);
        self.easing = easing;
        // 퍼센트 ↔ 남은 시간: 바뀌기 직전 값을 기억해 두고 겹쳐 그림
        let short_now = self.place.is_some_and(|p| p.fit == Fit::Short);
        if let Some(prev) = self.content.as_ref().filter(|c| c.time_mode != content.time_mode) {
            let from = prev
                .sections
                .iter()
                .enumerate()
                .flat_map(|(i, s)| s.rows.iter().map(move |r| ((i, r.key.clone()), r.shown(short_now).to_string())))
                .collect();
            self.switch = Some(Switch { from, start: now });
        }
        if self.switch_progress().is_some_and(|t| t >= 1.0) {
            self.switch = None;
        }
        let structure_changed = self.content.as_ref().is_none_or(|c| {
            c.sections.len() != content.sections.len()
                || c.rabbit.is_some() != content.rabbit.is_some()
                || c.sections.iter().zip(&content.sections).any(|(a, b)| columns(a.rows.len()) != columns(b.rows.len()))
        });
        self.content = Some(content);
        if structure_changed || resize {
            self.reposition();
        } else {
            self.paint();
        }
    }

    pub fn reposition(&mut self) {
        let place = if self.enabled { self.content.as_ref().and_then(|c| self.compute_place(c)) } else { None };
        let Some(place) = place else {
            if self.shown {
                unsafe { ShowWindow(self.hwnd, SW_HIDE) };
                self.shown = false;
            }
            self.place = None;
            return;
        };
        let moved = Some(place) != self.place;
        self.place = Some(place);
        if moved {
            self.last_key = None;
        }
        unsafe {
            SetWindowPos(self.hwnd, HWND_TOPMOST, place.x, place.y, place.w, place.h, SWP_NOACTIVATE);
        }
        self.paint();
        if !self.shown {
            unsafe { ShowWindow(self.hwnd, SW_SHOWNOACTIVATE) };
            self.shown = true;
        }
    }

    fn compute_place(&self, content: &Content) -> Option<Place> {
        if content.sections.is_empty() && content.rabbit.is_none() {
            return None; // 등록한 계정이 없거나 모두 꺼 둠
        }
        let tb = self.taskbar;
        if tb.is_null() || unsafe { IsWindow(tb) } == 0 || unsafe { IsWindowVisible(tb) } == 0 {
            return None;
        }
        let r = win::window_rect(tb)?;
        let (tb_w, tb_h) = (r.right - r.left, r.bottom - r.top);
        if tb_w < tb_h || tb_h <= 0 {
            return None; // 세로 작업 표시줄은 지원 안 함
        }
        let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if unsafe { GetMonitorInfoW(MonitorFromWindow(tb, MONITOR_DEFAULTTONEAREST), &mut mi) } == 0 {
            return None;
        }
        let mon = mi.rcMonitor;
        let visible_h = r.bottom.min(mon.bottom) - r.top.max(mon.top);
        if (visible_h as f32) < tb_h as f32 * 0.9 {
            return None; // 자동 숨김으로 내려가 있음
        }
        if fullscreen_app(&mon, self.hwnd) {
            return None;
        }
        let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForWindow(tb) };
        let scale = if dpi > 0 { dpi } else { win::taskbar_dpi() } as f32 / 96.0;
        let px = |v: f32| (v * scale).round() as i32;
        let full_w = px(self.width_for(content, Fit::Full));
        let compact_w = px(self.width_for(content, Fit::Compact));
        let short_w = px(self.width_for(content, Fit::Short));
        let margin = px(MARGIN);

        let child = |class: &str| unsafe {
            win::window_rect(FindWindowExW(tb, std::ptr::null_mut(), win::wide(class).as_ptr(), std::ptr::null()))
        };
        // 앱 버튼 영역: 주 작업 표시줄은 ReBarWindow32, 보조 작업 표시줄은 WorkerW
        let tasks = child("ReBarWindow32").or_else(|| child("WorkerW")).filter(|t| t.right > t.left);
        let start = child("Start");
        let tasks_left = [tasks, start].iter().flatten().map(|r| r.left).min().unwrap_or(r.left);
        // Windows 11은 버튼을 XAML로 그려 예전 창 위치가 실제와 다를 수 있음 → UI 자동화로 읽은 실제 끝을 우선
        let uia = crate::uia::get(tb as isize);
        let tasks_right = uia
            .and_then(|e| e.buttons_right)
            .map(|x| x.max(tasks.map(|t| t.right).unwrap_or(x)))
            .or(tasks.map(|x| x.right))
            .unwrap_or(r.left);

        let fit = |lo: i32, hi: i32, from_right: bool| -> Option<Place> {
            let avail = hi - lo;
            // 실제로 남는 자리에 맞춰: 막대 → 라벨+값 → 짧은 라벨+값 → 숨김
            let (w, fit) = if avail >= full_w {
                (full_w, Fit::Full)
            } else if avail >= compact_w {
                (compact_w, Fit::Compact)
            } else if avail >= short_w {
                (short_w, Fit::Short)
            } else {
                return None;
            };
            let x = if from_right { hi - w } else { lo };
            Some(Place { x, y: r.top, w, h: tb_h, scale, fit })
        };

        match self.side {
            Side::Left => {
                let mut lo = r.left + margin;
                let centered = win::reg_dword(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarAl") != Some(0);
                let widgets = win::reg_dword(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarDa") != Some(0);
                if self.primary && centered && widgets {
                    lo += px(170.0); // 왼쪽 끝 날씨(위젯) 버튼 자리
                }
                fit(lo, tasks_left - margin, false)
            }
            Side::Right => {
                let tray_left = if self.primary {
                    // 알림 영역(^ 포함)의 왼쪽 끝
                    child("TrayNotifyWnd").filter(|n| n.right > n.left).map(|n| n.left).unwrap_or(r.right - px(260.0))
                } else {
                    // 보조 작업 표시줄엔 알림 영역이 없고 오른쪽 끝에 시계·알림 종이 있음 (실제 위치를 알면 그걸로)
                    uia.and_then(|e| e.tray_left).filter(|&x| x > r.left && x < r.right).unwrap_or(r.right - px(124.0))
                };
                fit((tasks_right + margin).max(r.left), tray_left - margin, true)
            }
        }
    }

    fn paint(&mut self) {
        let (Some(place), Some(content)) = (self.place, self.content.clone()) else { return };
        let progress = self.switch_progress();
        let step = progress.map(|t| (t * 1000.0) as u32);
        let col = self.value_col.map(|c| (c * 10.0) as i64);
        let key = hash_of(&(format!("{:?}", content), place.w, place.h, place.fit, step, col));
        if Some(key) == self.last_key {
            return;
        }
        // 글자/막대/로고 레이아웃은 토끼 프레임과 따로 캐시
        let layout_key = hash_of(&(format!("{:?}{:?}", content.sections, content.ink), place.w, place.h, place.fit, step, col));
        let base = match &self.layout {
            Some((k, img)) if *k == layout_key => img.clone(),
            _ => {
                let from = self.switch.as_ref().map(|s| s.from.clone());
                let tr = from.as_ref().zip(progress);
                let img = self.draw_layout(&content, &place, tr);
                self.layout = Some((layout_key, img.clone()));
                img
            }
        };
        let mut img = base;
        if let Some(pose) = content.rabbit {
            let size = ((24.0 * place.scale).round() as u32).min(place.h as u32 - 2);
            if self.rabbits.len() > 400 {
                self.rabbits.clear();
            }
            let mask = self.rabbits.entry((pose, size)).or_insert_with(|| rabbit::render(&pose, size));
            let x = (PAD * place.scale).round() as i32 + ((RABBIT_W - 4.0) * place.scale - size as f32).max(0.0) as i32 / 2;
            img.blend_mask(x, (place.h - size as i32) / 2, mask, size, size, content.ink, 255);
        }
        win::blit_layered(self.hwnd, place.x, place.y, &img);
        self.last_key = Some(key);
    }

    /// 값들 중 가장 넓은 글자 너비 (논리 px)
    fn measure_col<'a>(&mut self, values: impl Iterator<Item = &'a str>, scale: f32) -> f32 {
        let px = (13.0 * scale).round() as i32;
        values.map(|v| self.text.measure(v, value_font(v, px)).0 as f32 / scale).fold(0.0, f32::max)
    }

    /// 값 글자. 전환 중이면 이전 값은 위로 빠지며 흐려지고, 새 값은 아래에서 올라오며 또렷해짐.
    #[allow(clippy::too_many_arguments)]
    fn draw_value(&mut self, img: &mut Image, right: i32, y: i32, val: &str, old: Option<&String>, t: Option<f32>, spec: FontSpec, ink: Rgb, slide: f32) {
        match (old, t) {
            (Some(old), Some(t)) if old != val => {
                // 나가는 값이 먼저 출발하고, 들어오는 값이 살짝 늦게 따라옴 (겹침 최소화)
                let out = ease_out_cubic(t / 0.55);
                let inn = ease_out_cubic((t - 0.18) / 0.82);
                let a_out = ((1.0 - out) * 255.0).round() as u8;
                let a_in = (inn * 255.0).round() as u8;
                if a_out > 0 {
                    self.text.draw(img, right, y - (out * slide).round() as i32, old, value_font(old, spec.px), ink, a_out, Align::Right);
                }
                if a_in > 0 {
                    self.text.draw(img, right, y + ((1.0 - inn) * slide).round() as i32, val, value_font(val, spec.px), ink, a_in, Align::Right);
                }
            }
            _ => {
                self.text.draw(img, right, y, val, value_font(val, spec.px), ink, 255, Align::Right);
            }
        }
    }

    fn draw_layout(&mut self, content: &Content, place: &Place, tr: Option<(&HashMap<(usize, String), String>, f32)>) -> Image {
        let s = place.scale;
        let px = |v: f32| (v * s).round() as i32;
        let slide = 7.0 * s;
        // 값 칸 너비: 앱에서는 부드럽게 따라가는 값, 미리 보기에서는 직접 잼
        let col = match self.value_col {
            Some(c) => c as f32,
            None => {
                let new = self.measure_col(content.sections.iter().flat_map(|s| s.rows.iter().map(|r| r.value.as_str())), s);
                match tr {
                    Some((from, t)) => {
                        let old = self.measure_col(from.values().map(|v| v.as_str()), s);
                        old + (new - old) * ease_out_cubic(t / 0.75)
                    }
                    None => new,
                }
            }
        };
        // 막대 = 열 너비 - 라벨 - 간격 - 지금 값 글자. 남은 시간 표시에서 설정 길이, 퍼센트 표시에서는 그보다 김
        let col_w = self.col_w;
        let label_span = self.label_span();
        let bar_len = (col_w - label_span - VALUE_GAP - col - 2.0).max(24.0);
        let (from, t) = (tr.map(|x| x.0), tr.map(|x| x.1));
        let mut img = Image::new(place.w as u32, place.h as u32);
        // 알파 1: 눈에는 안 보이지만 마우스는 받도록
        img.fill(content.ink, 1);
        let h = place.h;
        let mut x = PAD + if content.rabbit.is_some() { RABBIT_W } else { 0.0 };
        let label = FontSpec { px: px(12.0), weight: 700, face: "Malgun Gothic" };
        let value = FontSpec::value(px(13.0));
        for (i, sec) in content.sections.iter().enumerate() {
            if i > 0 {
                // 칸 사이 구분선
                let divider = if place.fit == Fit::Full { DIVIDER_W } else { COMPACT_DIVIDER };
                let dx = x + divider / 2.0;
                let line = vec![255u8; (px(1.0).max(1) * (h / 2)) as usize];
                img.blend_mask(px(dx), h / 4, &line, px(1.0).max(1) as u32, (h / 2) as u32, content.ink, 50);
                x += divider;
            }
            let x0 = px(x);
            let logo_size = if place.fit == Fit::Full { px(18.0) } else { px(16.0) } as u32;
            let logo = self.logos.entry((sec.id, logo_size)).or_insert_with(|| logos::render(sec.id, logo_size)).clone();
            let color = logos::brand_color(sec.id).unwrap_or(content.ink);
            img.blend_mask(x0, (h - logo_size as i32) / 2, &logo, logo_size, logo_size, color, 255);

            // 두 줄씩 열로 나눔 (세 번째 줄부터 같은 칸의 다음 열)
            for (ci, chunk) in sec.rows.chunks(2).enumerate() {
                let ys: Vec<i32> = match chunk.len() {
                    1 => vec![h / 2],
                    _ => vec![(h as f32 * 0.31).round() as i32, (h as f32 * 0.70).round() as i32],
                };
                if place.fit != Fit::Full {
                    // 막대 없이 라벨 + 값 (아주 좁으면 5h·7d 짧은 라벨)
                    let short = place.fit == Fit::Short;
                    let lab_w = if short { self.label_cols.1 } else { self.label_cols.0 };
                    let vcol = if short { self.short_col } else { col };
                    let cx = x + COMPACT_LOGO + ci as f32 * (lab_w + COMPACT_GAP + vcol + COMPACT_COL_GAP);
                    let right = px(cx + lab_w + COMPACT_GAP + vcol);
                    for (y, row) in ys.iter().zip(chunk) {
                        let text = if short { short_label(&row.key) } else { row.label.clone() };
                        self.text.draw(&mut img, px(cx), *y, &text, label, content.ink, 235, Align::Left);
                        let old = from.and_then(|f| f.get(&(i, row.key.clone())));
                        let shown = row.shown(short).to_string();
                        self.draw_value(&mut img, right, *y, &shown, old, t, value, content.ink, slide);
                    }
                    continue;
                }
                let cx = x + LOGO_COL + ci as f32 * (col_w + COL_GAP);
                let (bar_x, bar_w, bar_h) = (px(cx + label_span), px(bar_len), px(5.0).max(4));
                let right = px(cx + col_w - 2.0);
            for (y, row) in ys.iter().zip(chunk) {
                let (lab, val, ratio) = (&row.label, &row.value, &row.ratio);
                self.text.draw(&mut img, px(cx), *y, lab, label, content.ink, 235, Align::Left);
                let track = bar_mask(bar_w as u32, bar_h as u32, bar_w as u32);
                img.blend_mask(bar_x, y - bar_h / 2, &track, bar_w as u32, bar_h as u32, content.ink, 55);
                // 남은 양 기준이면 막대가 줄어드는 방향 (색은 사용량 기준 그대로)
                let shown = if row.left { (1.0 - ratio).max(0.0) } else { *ratio };
                if shown > 0.0 && val != "–" {
                    let fill_w = ((bar_w as f64 * shown.min(1.0)).round() as u32).max(bar_h as u32);
                    let fill = bar_mask(bar_w as u32, bar_h as u32, fill_w);
                    img.blend_mask(bar_x, y - bar_h / 2, &fill, bar_w as u32, bar_h as u32, BAR_COLORS[texts::level(*ratio) as usize], 255);
                }
                let old = from.and_then(|f| f.get(&(i, row.key.clone())));
                self.draw_value(&mut img, right, *y, val, old, t, value, content.ink, slide);
            }
            }
            let (lab_w, vcol) = if place.fit == Fit::Short { (self.label_cols.1, self.short_col) } else { (self.label_cols.0, col) };
            x += section_width(sec.rows.len(), place.fit, lab_w, vcol, col_w);
        }
        img
    }
}

/// 디자인 확인용: 창 없이 위젯 이미지를 그림
/// level: 0 넓음(막대), 1 좁음(라벨+값), 2 아주 좁음(짧은 라벨+값)
pub fn preview(content: &Content, scale: f32, height: i32, level: u8) -> Image {
    preview_bar(content, scale, height, level, None, None)
}

pub fn preview_switch(content: &Content, scale: f32, height: i32, level: u8, tr: Option<(&HashMap<(usize, String), String>, f32)>) -> Image {
    preview_bar(content, scale, height, level, tr, None)
}

/// 디자인 확인용: 퍼센트 ↔ 남은 시간 전환의 한 장면 (from: (칸, 그래프 key) → 이전 값, t: 0..1)
/// 막대 길이 설정(bar)을 정해서 그리기
pub fn preview_bar(content: &Content, scale: f32, height: i32, level: u8, tr: Option<(&HashMap<(usize, String), String>, f32)>, bar_px: Option<f32>) -> Image {
    let mut w = Widget::new(std::ptr::null_mut(), std::ptr::null_mut(), true);
    w.bar_px = bar_px;
    w.label_cols = w.measure_labels(content, scale); // 열 너비가 라벨 칸을 따르므로 먼저
    w.col_w = w.full_col_width(scale);
    let fit = match level {
        0 => Fit::Full,
        1 => Fit::Compact,
        _ => Fit::Short,
    };
    let col = w.measure_col(content.sections.iter().flat_map(|s| s.rows.iter().map(|r| r.value.as_str())), scale);
    if tr.is_none() {
        w.value_col = Some(col as f64);
    }
    w.short_col = w.measure_col(content.sections.iter().flat_map(|s| s.rows.iter().map(|r| r.shown(true))), scale);
    let width = (w.width_for(content, fit) * scale).round() as i32;
    let place = Place { x: 0, y: 0, w: width, h: height, scale, fit };
    let mut img = w.draw_layout(content, &place, tr);
    if let Some(pose) = content.rabbit {
        let size = ((24.0 * scale).round() as u32).min(height as u32 - 2);
        let mask = rabbit::render(&pose, size);
        img.blend_mask((PAD * scale).round() as i32, (height - size as i32) / 2, &mask, size, size, content.ink, 255);
    }
    img
}

/// 값 글꼴: 숫자·%는 Segoe UI, 한글이 섞인 남은 시간(6일 23시간)은 라벨과 같은 맑은 고딕 굵게
/// (Segoe UI로 그리면 한글만 가는 대체 글꼴로 나와 굵기가 섞여 보임)
fn value_font(text: &str, px: i32) -> FontSpec {
    if text.is_ascii() {
        FontSpec::value(px)
    } else {
        FontSpec { px, weight: 700, face: "Malgun Gothic" }
    }
}

/// 둥근 끝 막대 마스크 (w x h 캔버스에 왼쪽부터 fill_w 만큼)
fn bar_mask(w: u32, h: u32, fill_w: u32) -> Vec<u8> {
    let Some(mut pm) = Pixmap::new(w.max(1), h.max(1)) else { return vec![0; (w * h) as usize] };
    let r = h as f32 / 2.0;
    let mut pb = PathBuilder::new();
    pb.move_to(r, r);
    pb.line_to((fill_w as f32 - r).max(r + 0.01), r);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.anti_alias = true;
        paint.set_color_rgba8(255, 255, 255, 255);
        let stroke = Stroke { width: h as f32, line_cap: LineCap::Round, ..Default::default() };
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
    pm.data().chunks_exact(4).map(|p| p[3]).collect()
}

/// 이 모니터에서 전체 화면 앱(영상·게임·발표)이 앞에 있는지.
///
/// `SHQueryUserNotificationState`의 QUNS_BUSY는 쓰지 않음: 화면 전체를 덮는 투명·클릭 통과 오버레이
/// (녹화·게임 오버레이, 데스크톱 펫, 원격 제어 테두리 등)만 떠 있어도 BUSY가 되고, 모니터 구분도 없어서
/// 모든 모니터의 위젯이 사라졌음. 그래서 앞 창이 이 모니터를 덮는지로 판단하고, 클릭 통과 창은 무시.
fn fullscreen_app(mon: &RECT, own: HWND) -> bool {
    let mut state: QUERY_USER_NOTIFICATION_STATE = 0;
    if unsafe { SHQueryUserNotificationState(&mut state) } == 0 && matches!(state, QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE) {
        return true;
    }
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_null() || fg == own {
        return false;
    }
    if matches!(win::class_name(fg).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
        return false;
    }
    let ex = unsafe { GetWindowLongPtrW(fg, GWL_EXSTYLE) } as u32;
    if ex & WS_EX_TRANSPARENT != 0 {
        return false; // 클릭 통과 오버레이
    }
    win::window_rect(fg).is_some_and(|r| r.left <= mon.left && r.top <= mon.top && r.right >= mon.right && r.bottom >= mon.bottom)
}
