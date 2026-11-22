//! 화면에 보이는 한국어 문구와 시간 표기.

use crate::engine::{Meter, ProviderView, Snapshot};
use crate::logs::LimitKind;
use crate::time;
use crate::tracker::{ForecastKind, Mood, Status};

// 문구는 꾸밈말 없이 사실만 적어요 (한도 도달, 리셋, 사용량).

pub const APP_NAME: &str = "usAgI";

const WD: [&str; 7] = ["월", "화", "수", "목", "금", "토", "일"];

/// 2시간 13분 / 13분 / 1일 3시간 / 1분 미만
pub fn fmt_duration(seconds: f64) -> String {
    if seconds < 60.0 {
        return "1분 미만".into();
    }
    let minutes = ((seconds + 59.0) / 60.0) as i64;
    let (days, rem) = (minutes / 1440, minutes % 1440);
    let (hours, mins) = (rem / 60, rem % 60);
    match (days, hours, mins) {
        (d, 0, _) if d > 0 => format!("{d}일"),
        (d, h, _) if d > 0 => format!("{d}일 {h}시간"),
        (_, h, 0) if h > 0 => format!("{h}시간"),
        (_, h, m) if h > 0 => format!("{h}시간 {m}분"),
        (_, _, m) => format!("{m}분"),
    }
}

/// 아주 좁은 위젯용 짧은 남은 시간: 6d23h / 5h23m / 45m
pub fn fmt_hm_short(seconds: f64) -> String {
    let minutes = ((seconds.max(0.0) + 59.0) / 60.0) as i64;
    let (days, rem) = (minutes / 1440, minutes % 1440);
    let (hours, mins) = (rem / 60, rem % 60);
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{mins}m")
    } else {
        format!("{mins}m")
    }
}

/// 위젯의 남은 시간: 6일 23시간 / 2시간 13분 / 45분 (단위를 항상 같이 적어 자리가 덜 흔들림)
pub fn fmt_hm(seconds: f64) -> String {
    let minutes = ((seconds.max(0.0) + 59.0) / 60.0) as i64;
    let (days, rem) = (minutes / 1440, minutes % 1440);
    let (hours, mins) = (rem / 60, rem % 60);
    if days > 0 {
        format!("{days}일 {hours}시간")
    } else if hours > 0 {
        format!("{hours}시간 {mins}분")
    } else {
        format!("{mins}분")
    }
}

/// 오후 4시 20분 / 내일 오전 9시 / 11월 25일(수) 오전 9시
pub fn fmt_clock(ts: f64, now: f64) -> String {
    let t = time::local(ts);
    let n = time::local(now);
    let clock = match (t.hour, t.minute) {
        (0, 0) => "자정".to_string(),
        (12, 0) => "정오".to_string(),
        (h, m) => {
            let ampm = match h {
                0 => "밤",
                12 => "낮",
                h if h < 12 => "오전",
                _ => "오후",
            };
            let h12 = if h % 12 == 0 { 12 } else { h % 12 };
            if m == 0 {
                format!("{ampm} {h12}시")
            } else {
                format!("{ampm} {h12}시 {m}분")
            }
        }
    };
    match t.day_number() - n.day_number() {
        0 => clock,
        1 => format!("내일 {clock}"),
        2 => format!("모레 {clock}"),
        _ => format!("{}월 {}일({}) {clock}", t.month, t.day, WD[t.weekday as usize]),
    }
}

pub fn fmt_pct(ratio: f64) -> String {
    if ratio > 1.0 {
        "100%+".into()
    } else {
        format!("{}%", (ratio * 100.0).round() as i64)
    }
}

/// 막대 색 단계: 0 여유(초록), 1 주의(주황), 2 위험(빨강)
pub fn level(ratio: f64) -> u8 {
    if ratio >= 0.8 {
        2
    } else if ratio >= 0.5 {
        1
    } else {
        0
    }
}

pub fn forecast_line(st: &Status) -> String {
    if st.mood == Mood::Nap {
        return String::new();
    }
    // 쓰고 있지 않으면 예상할 게 없음
    let Some(f) = st.forecast else { return String::new() };
    if f.safe {
        return "이 속도면 리셋 전까지 한도 여유".into();
    }
    let suffix = if f.kind == ForecastKind::Weekly { " (주간 한도)" } else { "" };
    if f.at - st.now < 60.0 {
        format!("곧 한도 도달{suffix}")
    } else {
        format!("이 속도면 {}에 한도 도달{suffix}", fmt_clock(f.at, st.now))
    }
}

/// 리셋까지 남은 시간
pub fn wake_line(st: &Status) -> String {
    if let (Mood::Nap, Some(w)) = (st.mood, st.wake_at) {
        return format!("한도 도달 · {} 뒤 리셋 ({})", fmt_duration(w - st.now), fmt_clock(w, st.now));
    }
    match st.session_reset_at.filter(|_| st.session_active()) {
        Some(r) => format!("세션 리셋 {} 뒤", fmt_duration(r - st.now)),
        None => "다음 사용부터 5시간 세션 시작".into(),
    }
}

pub fn provider_line(p: &ProviderView) -> String {
    if !p.meters.iter().any(|m| m.exact) {
        return format!("{} · 연결 필요", p.name);
    }
    let parts: Vec<String> =
        p.meters.iter().map(|m| format!("{} {}", m.label, fmt_pct(m.ratio))).collect();
    format!("{} {}", p.name, parts.join(" · "))
}

/// 트레이 아이콘 툴팁 (Windows 한도 127자)
pub fn tooltip(snap: Option<&Snapshot>) -> String {
    let Some(snap) = snap else { return format!("{APP_NAME} · 불러오는 중…") };
    let mut lines = Vec::new();
    match &snap.claude {
        Some(st) if st.has_data => {
            lines.push(APP_NAME.to_string());
            for p in snap.visible() {
                lines.push(provider_line(p));
            }
            let fl = forecast_line(st);
            if !fl.is_empty() {
                lines.push(fl);
            }
            lines.push(wake_line(st));
        }
        _ => {
            lines.push(APP_NAME.to_string());
            for p in &snap.providers {
                lines.push(provider_line(p));
            }
            if snap.providers.is_empty() {
                lines.push("사용 기록 없음".into());
            }
        }
    }
    let text = lines.join("\n");
    if text.chars().count() > 127 {
        text.chars().take(126).collect::<String>() + "…"
    } else {
        text
    }
}

pub fn menu_lines(snap: Option<&Snapshot>) -> Vec<String> {
    let Some(snap) = snap else { return vec!["불러오는 중…".into()] };
    let mut out = Vec::new();
    for p in &snap.providers {
        for m in &p.meters {
            let mut line = format!("{} {} {}", p.name, m.label, if m.exact { fmt_pct(m.ratio) } else { "–".into() });
            if let Some(r) = m.resets_at {
                line += &format!(" · {} 리셋", fmt_clock(r, snap.now));
            }
            out.push(line);
        }
        if let Some(err) = &p.note {
            out.push(format!("{} · {err}", p.name));
        }
    }
    if let Some(st) = snap.claude.as_ref().filter(|s| s.has_data) {
        let fl = forecast_line(st);
        if !fl.is_empty() {
            out.push(fl);
        }
        out.push(wake_line(st));
    }
    if out.is_empty() {
        out.push("사용 기록 없음".into());
    }
    out
}

/// 위젯 숫자 표시: 퍼센트 또는 리셋까지 남은 시간 (우클릭으로 전환)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueMode {
    Percent,
    Remaining,
}

pub fn meter_value(m: &Meter, mode: ValueMode, now: f64) -> String {
    if !m.exact {
        return "–".into(); // 추정치는 보여 주지 않음
    }
    match mode {
        ValueMode::Percent => fmt_pct(m.ratio),
        ValueMode::Remaining => m.resets_at.filter(|&r| r > now).map(|r| fmt_hm(r - now)).unwrap_or_else(|| "–".into()),
    }
}

pub fn tired_notice(name: &str, label: &str, ratio: f64, st: Option<&Status>) -> (String, String) {
    let mut msg = format!("{name} {label} 한도의 {} 사용.", fmt_pct(ratio));
    if let Some(st) = st {
        if st.forecast.is_some_and(|f| !f.safe) {
            msg += &format!(" {}.", forecast_line(st));
        }
    }
    ("사용량 알림".into(), msg)
}

pub fn nap_notice(st: &Status) -> (String, String) {
    let what = if st.nap_kind == Some(LimitKind::Weekly) { "주간 한도" } else { "세션 한도" };
    let mut msg = format!("{what} 도달.");
    if let Some(w) = st.wake_at {
        msg += &format!(" {} 뒤({}) 리셋.", fmt_duration(w - st.now), fmt_clock(w, st.now));
    }
    ("한도 도달".into(), msg)
}

pub fn wake_notice() -> (String, String) {
    ("한도 리셋".into(), "한도 리셋 완료. 다시 사용 가능.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(2.0 * 3600.0 + 13.0 * 60.0), "2시간 13분");
        assert_eq!(fmt_duration(30.0), "1분 미만");
        assert_eq!(fmt_duration(26.0 * 3600.0), "1일 2시간");
        assert_eq!(fmt_hm(2.0 * 3600.0 + 13.0 * 60.0), "2시간 13분");
        assert_eq!(fmt_hm(2.0 * 3600.0), "2시간 0분");
        assert_eq!(fmt_hm(45.0 * 60.0), "45분");
        assert_eq!(fmt_hm(6.0 * 86400.0 + 23.0 * 3600.0 + 10.0 * 60.0), "6일 23시간");
        assert_eq!(fmt_hm(86400.0), "1일 0시간");
        assert_eq!(fmt_hm_short(5.0 * 3600.0 + 23.0 * 60.0), "5h23m");
        assert_eq!(fmt_hm_short(6.0 * 86400.0 + 23.0 * 3600.0 + 10.0), "6d23h");
        assert_eq!(fmt_hm_short(45.0 * 60.0), "45m");
    }

    #[test]
    fn clocks() {
        let at = |d, h, m| time::local_to_ts(2026, 11, d, h, m).unwrap();
        let now = at(21, 9, 0);
        assert_eq!(fmt_clock(at(21, 16, 20), now), "오후 4시 20분");
        assert_eq!(fmt_clock(at(22, 9, 0), now), "내일 오전 9시");
        assert_eq!(fmt_clock(at(22, 0, 11), now), "내일 밤 12시 11분");
        assert_eq!(fmt_clock(at(25, 9, 0), now), "11월 25일(수) 오전 9시");
    }

    #[test]
    fn pct_and_levels() {
        assert_eq!(fmt_pct(0.374), "37%");
        assert_eq!(fmt_pct(1.2), "100%+");
        assert_eq!((level(0.2), level(0.6), level(0.85)), (0, 1, 2));
    }
}
