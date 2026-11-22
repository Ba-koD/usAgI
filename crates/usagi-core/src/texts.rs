//! 화면에 보이는 한국어 문구와 시간 표기.

use crate::engine::{Meter, ProviderView, Snapshot};
use crate::logs::LimitKind;
use crate::time;
use crate::tracker::{ForecastKind, Mood, Status};

// 문구는 꾸밈말 없이 사실만 적어요 (한도 도달, 리셋, 사용량).

pub const APP_NAME: &str = "usAgI";

/// 화면에 보일 그래프 이름. 저장 키는 한국어("5시간", "주간", "Fable 주간")라 보일 때만 바꿈
pub fn meter_label(label: &str) -> String {
    if !crate::i18n::is_en() {
        return label.to_string();
    }
    match label {
        "5시간" => "5-hour".into(),
        "주간" => "Weekly".into(),
        other => match other.strip_suffix(" 주간") {
            Some(model) => format!("{model} weekly"),
            // Codex의 다른 길이 창: "3시간" → "3-hour", "2일" → "2-day", "90분" → "90-min"
            None => [("시간", "hour"), ("일", "day"), ("분", "min")]
                .iter()
                .find_map(|(ko, en)| {
                    other
                        .strip_suffix(ko)
                        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                        .map(|n| format!("{n}-{en}"))
                })
                .unwrap_or_else(|| other.to_string()),
        },
    }
}

/// 문장 안의 그래프 이름 (영어는 "weekly"처럼 소문자)
fn label_in_sentence(label: &str) -> String {
    let shown = meter_label(label);
    if crate::i18n::is_en() && label == "주간" {
        shown.to_lowercase()
    } else {
        shown
    }
}

const WD: [&str; 7] = ["월", "화", "수", "목", "금", "토", "일"];
const WD_EN: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTH_EN: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// 시간 단위: 일·시간·분 / d·h·m
fn units() -> (&'static str, &'static str, &'static str) {
    crate::tr!(("일", "시간", "분"), ("d", "h", "m"))
}

/// 2시간 13분 / 13분 / 1일 3시간 / 1분 미만 (영어: 2h 13m / 13m / 1d 3h / under 1m)
pub fn fmt_duration(seconds: f64) -> String {
    if seconds < 60.0 {
        return crate::tr!("1분 미만", "under 1m").into();
    }
    let minutes = ((seconds + 59.0) / 60.0) as i64;
    let (days, rem) = (minutes / 1440, minutes % 1440);
    let (hours, mins) = (rem / 60, rem % 60);
    let (du, hu, mu) = units();
    match (days, hours, mins) {
        (d, 0, _) if d > 0 => format!("{d}{du}"),
        (d, h, _) if d > 0 => format!("{d}{du} {h}{hu}"),
        (_, h, 0) if h > 0 => format!("{h}{hu}"),
        (_, h, m) if h > 0 => format!("{h}{hu} {m}{mu}"),
        (_, _, m) => format!("{m}{mu}"),
    }
}

/// 알림 시점(리셋 몇 분 전) 표기: 30분 / 1시간 / 1시간 30분 / 1일 / 1일 2시간 (영어: 30m / 1h / 1h 30m / 1d / 1d 2h).
/// 0이 아닌 단위는 모두 적음 (1일 30분)
pub fn fmt_lead(minutes: f64) -> String {
    let total = minutes.max(0.0).round() as i64;
    let (du, hu, mu) = units();
    let parts: Vec<String> = [(total / 1440, du), (total % 1440 / 60, hu), (total % 60, mu)]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, u)| format!("{n}{u}"))
        .collect();
    if parts.is_empty() {
        format!("0{mu}")
    } else {
        parts.join(" ")
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
/// 영어: 6d 23h / 2h 13m / 45m
pub fn fmt_hm(seconds: f64) -> String {
    let minutes = ((seconds.max(0.0) + 59.0) / 60.0) as i64;
    let (days, rem) = (minutes / 1440, minutes % 1440);
    let (hours, mins) = (rem / 60, rem % 60);
    let (du, hu, mu) = units();
    if days > 0 {
        format!("{days}{du} {hours}{hu}")
    } else if hours > 0 {
        format!("{hours}{hu} {mins}{mu}")
    } else {
        format!("{mins}{mu}")
    }
}

/// 영어 시각: 4:20 PM / 12:05 AM
fn clock_en(hour: u32, minute: u32) -> String {
    let h12 = if hour % 12 == 0 { 12 } else { hour % 12 };
    format!("{h12}:{minute:02} {}", if hour < 12 { "AM" } else { "PM" })
}

/// 영어 날짜: Nov 25 (Wed)
fn date_en(t: &time::Parts) -> String {
    format!("{} {} ({})", MONTH_EN[(t.month as usize + 11) % 12], t.day, WD_EN[t.weekday as usize % 7])
}

/// 오후 4시 20분 / 내일 오전 9시 / 11월 25일(수) 오전 9시
/// 영어: 4:20 PM / Tomorrow 9:00 AM / Nov 25 (Wed) 9:00 AM
pub fn fmt_clock(ts: f64, now: f64) -> String {
    let t = time::local(ts);
    let n = time::local(now);
    if crate::i18n::is_en() {
        let clock = clock_en(t.hour, t.minute);
        return match t.day_number() - n.day_number() {
            0 => clock,
            1 => format!("Tomorrow {clock}"),
            _ => format!("{} {clock}", date_en(&t)),
        };
    }
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

/// 영어 문장 속 시각: at 4:20 PM / tomorrow at 2:10 AM / on Nov 29 (Sun) at 10:00 AM
fn when_en(ts: f64, now: f64) -> String {
    let t = time::local(ts);
    let n = time::local(now);
    let clock = clock_en(t.hour, t.minute);
    match t.day_number() - n.day_number() {
        0 => format!("at {clock}"),
        1 => format!("tomorrow at {clock}"),
        _ => format!("on {} at {clock}", date_en(&t)),
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
        return crate::tr!("이 속도면 리셋 전까지 한도에 도달하지 않음", "At this pace, the limit holds until reset").into();
    }
    let weekly = f.kind == ForecastKind::Weekly;
    if crate::i18n::is_en() {
        let what = if weekly { "weekly limit" } else { "limit" };
        return if f.at - st.now < 60.0 {
            format!("{} expected soon", if weekly { "Weekly limit" } else { "Limit" })
        } else {
            format!("At this pace, {what} expected {}", when_en(f.at, st.now))
        };
    }
    let suffix = if weekly { " (주간 한도)" } else { "" };
    if f.at - st.now < 60.0 {
        format!("곧 한도 도달{suffix}")
    } else {
        format!("이 속도면 {}에 한도 도달{suffix}", fmt_clock(f.at, st.now))
    }
}

/// 리셋까지 남은 시간
pub fn wake_line(st: &Status) -> String {
    if let (Mood::Nap, Some(w)) = (st.mood, st.wake_at) {
        let (left, at) = (fmt_duration(w - st.now), fmt_clock(w, st.now));
        return crate::tr!(format!("한도 도달 · {left} 뒤 리셋 ({at})"), format!("Limit reached · resets in {left} ({at})"));
    }
    match st.session_reset_at.filter(|_| st.session_active()) {
        Some(r) => {
            let left = fmt_duration(r - st.now);
            crate::tr!(format!("{left} 뒤 세션 리셋"), format!("Session resets in {left}"))
        }
        None => crate::tr!("다음 사용부터 5시간 세션 시작", "5-hour session starts with next use").into(),
    }
}

pub fn provider_line(p: &ProviderView) -> String {
    if !p.meters.iter().any(|m| m.exact) {
        return crate::tr!(format!("{} · 연결 필요", p.name), format!("{} · Not connected", p.name));
    }
    let parts: Vec<String> =
        p.meters.iter().map(|m| format!("{} {}", meter_label(&m.label), fmt_pct(m.ratio))).collect();
    format!("{} {}", p.name, parts.join(" · "))
}

/// 트레이 아이콘 툴팁 (Windows 한도 127자)
pub fn tooltip(snap: Option<&Snapshot>) -> String {
    let Some(snap) = snap else { return format!("{APP_NAME} · {}", crate::tr!("불러오는 중…", "Loading…")) };
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
                lines.push(crate::tr!("사용 기록 없음", "No usage data").into());
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
    let Some(snap) = snap else { return vec![crate::tr!("불러오는 중…", "Loading…").into()] };
    let mut out = Vec::new();
    for p in &snap.providers {
        for m in &p.meters {
            let mut line =
                format!("{} {} {}", p.name, meter_label(&m.label), if m.exact { fmt_pct(m.ratio) } else { "–".into() });
            if let Some(r) = m.resets_at {
                line += &crate::tr!(format!(" · {} 리셋", fmt_clock(r, snap.now)), format!(" · resets {}", when_en(r, snap.now)));
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
        out.push(crate::tr!("사용 기록 없음", "No usage data").into());
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
    let mut msg = crate::tr!(
        format!("{name} {label} 한도의 {} 사용.", fmt_pct(ratio)),
        format!("{name} {} limit {} used.", label_in_sentence(label), fmt_pct(ratio))
    );
    if let Some(st) = st {
        if st.forecast.is_some_and(|f| !f.safe) {
            msg += &format!(" {}.", forecast_line(st));
        }
    }
    (crate::tr!("사용량 알림", "Usage alert").into(), msg)
}

/// 리셋 임박 알림: "Claude 5시간 한도 30분 뒤 리셋 (오후 4시 20분)"
pub fn soon_notice(name: &str, label: &str, resets_at: f64, now: f64) -> (String, String) {
    let (left, clock) = (fmt_duration(resets_at - now), fmt_clock(resets_at, now));
    (
        crate::tr!("리셋 임박", "Reset soon").into(),
        crate::tr!(
            format!("{name} {} 한도 {left} 뒤 리셋 ({clock})", meter_label(label)),
            format!("{name} {} limit resets in {left} ({clock})", label_in_sentence(label))
        ),
    )
}

pub fn nap_notice(st: &Status) -> (String, String) {
    let what = if st.nap_kind == Some(LimitKind::Weekly) {
        crate::tr!("주간 한도", "Weekly limit")
    } else {
        crate::tr!("세션 한도", "Session limit")
    };
    let mut msg = crate::tr!(format!("{what} 도달."), format!("{what} reached."));
    if let Some(w) = st.wake_at {
        let (left, at) = (fmt_duration(w - st.now), fmt_clock(w, st.now));
        msg += &crate::tr!(format!(" {left} 뒤({at}) 리셋."), format!(" Resets in {left} ({at})."));
    }
    (crate::tr!("한도 도달", "Limit reached").into(), msg)
}

pub fn wake_notice() -> (String, String) {
    (
        crate::tr!("한도 리셋", "Limit reset").into(),
        crate::tr!("한도 리셋 완료. 다시 사용 가능.", "The limit has reset and usage is available again.").into(),
    )
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

    /// 이 스레드만 영어로, 끝나면 (실패해도) 한국어로 되돌림
    struct English;
    impl English {
        fn on() -> Self {
            crate::i18n::set(crate::i18n::Lang::En);
            English
        }
    }
    impl Drop for English {
        fn drop(&mut self) {
            crate::i18n::set(crate::i18n::Lang::Ko);
        }
    }

    #[test]
    fn english_durations() {
        let _en = English::on();
        assert_eq!(fmt_duration(2.0 * 3600.0 + 13.0 * 60.0), "2h 13m");
        assert_eq!(fmt_duration(30.0), "under 1m");
        assert_eq!(fmt_duration(26.0 * 3600.0), "1d 2h");
        assert_eq!(fmt_duration(3.0 * 86400.0), "3d");
        assert_eq!(fmt_duration(2.0 * 3600.0), "2h");
        assert_eq!(fmt_duration(13.0 * 60.0), "13m");
        assert_eq!(fmt_hm(2.0 * 3600.0 + 13.0 * 60.0), "2h 13m");
        assert_eq!(fmt_hm(2.0 * 3600.0), "2h 0m");
        assert_eq!(fmt_hm(45.0 * 60.0), "45m");
        assert_eq!(fmt_hm(6.0 * 86400.0 + 23.0 * 3600.0 + 10.0 * 60.0), "6d 23h");
        assert_eq!(fmt_hm(86400.0), "1d 0h");
        assert_eq!(fmt_hm_short(5.0 * 3600.0 + 23.0 * 60.0), "5h23m");
        assert_eq!(fmt_pct(0.374), "37%");
    }

    #[test]
    fn english_clocks() {
        let _en = English::on();
        let at = |d, h, m| time::local_to_ts(2026, 11, d, h, m).unwrap();
        let now = at(21, 9, 0);
        assert_eq!(fmt_clock(at(21, 16, 20), now), "4:20 PM");
        assert_eq!(fmt_clock(at(21, 12, 0), now), "12:00 PM");
        assert_eq!(fmt_clock(at(22, 2, 10), now), "Tomorrow 2:10 AM");
        assert_eq!(fmt_clock(at(22, 0, 11), now), "Tomorrow 12:11 AM");
        assert_eq!(fmt_clock(at(22, 0, 0), now), "Tomorrow 12:00 AM");
        assert_eq!(fmt_clock(at(23, 9, 5), now), "Nov 23 (Mon) 9:05 AM");
        assert_eq!(fmt_clock(at(29, 10, 0), now), "Nov 29 (Sun) 10:00 AM");
        assert_eq!(when_en(at(21, 16, 20), now), "at 4:20 PM");
        assert_eq!(when_en(at(22, 2, 10), now), "tomorrow at 2:10 AM");
        assert_eq!(when_en(at(29, 10, 0), now), "on Nov 29 (Sun) at 10:00 AM");
    }

    #[test]
    fn english_labels_and_notices() {
        let _en = English::on();
        assert_eq!(meter_label("5시간"), "5-hour");
        assert_eq!(meter_label("주간"), "Weekly");
        assert_eq!(meter_label("Fable 주간"), "Fable weekly");
        assert_eq!(meter_label("3시간"), "3-hour");
        assert_eq!(meter_label("2일"), "2-day");
        assert_eq!(meter_label("90분"), "90-min");
        assert_eq!(meter_label("Opus"), "Opus");
        assert_eq!(tired_notice("Codex", "주간", 0.8, None), ("Usage alert".into(), "Codex weekly limit 80% used.".into()));
        assert_eq!(tired_notice("Claude", "5시간", 0.5, None).1, "Claude 5-hour limit 50% used.");
        assert_eq!(wake_notice().0, "Limit reset");
        assert_eq!(tooltip(None), "usAgI · Loading…");
        assert_eq!(menu_lines(None), vec!["Loading…".to_string()]);
    }

    #[test]
    fn alert_leads_and_reset_soon_notices() {
        let leads = |list: &[f64]| list.iter().map(|&m| fmt_lead(m)).collect::<Vec<_>>();
        let mins = [30.0, 60.0, 90.0, 1440.0, 1560.0, 1470.0];
        assert_eq!(leads(&mins), ["30분", "1시간", "1시간 30분", "1일", "1일 2시간", "1일 30분"]);
        let at = |d, h, m| time::local_to_ts(2026, 11, d, h, m).unwrap();
        let now = at(21, 9, 0);
        assert_eq!(
            soon_notice("Codex", "주간", at(21, 9, 30), now),
            ("리셋 임박".into(), "Codex 주간 한도 30분 뒤 리셋 (오전 9시 30분)".into())
        );
        let _en = English::on();
        assert_eq!(leads(&mins), ["30m", "1h", "1h 30m", "1d", "1d 2h", "1d 30m"]);
        assert_eq!(
            soon_notice("Codex", "주간", at(21, 9, 30), now),
            ("Reset soon".into(), "Codex weekly limit resets in 30m (9:30 AM)".into())
        );
        assert_eq!(soon_notice("Claude", "5시간", at(21, 11, 0), now).1, "Claude 5-hour limit resets in 2h (11:00 AM)");
    }

    #[test]
    fn korean_labels_stay_as_stored() {
        assert_eq!(meter_label("3시간"), "3시간");
        assert_eq!(tired_notice("Codex", "주간", 0.8, None), ("사용량 알림".into(), "Codex 주간 한도의 80% 사용.".into()));
    }
}
