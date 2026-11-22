//! 토끼 상태 — 추정 없이, 실제로 아는 것만으로.
//!
//! - 사용률·리셋 시각: 서버가 준 정확한 값만 (엔진이 [`apply_exact`]로 채움). 없으면 0 + 표시는 "–".
//! - 쓰는 중인지(뛰는 속도): Claude Code 로컬 기록의 최근 토큰 흐름.
//! - 한도 도달(낮잠): 서버가 돌려준 거절 메시지("You've hit your session limit · resets 9:40pm").
//! - 낮잠 예보: 정확한 사용률이 시간에 따라 오른 속도로 계산.

use crate::bridge::BridgeFile;
use crate::logs::{LimitEvent, LimitKind, UsageEvent};
use crate::time::{DAY, HOUR};

pub const SESSION_SECONDS: f64 = 5.0 * HOUR;
pub const WEEK_SECONDS: f64 = 7.0 * DAY;
pub const ACTIVE_WINDOW: f64 = 3.0 * 60.0;
pub const LONG_IDLE: f64 = 20.0 * 60.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mood {
    Rest,
    Hop,
    Tired,
    Nap,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ForecastKind {
    Session,
    Weekly,
}

#[derive(Clone, Copy, Debug)]
pub struct Forecast {
    pub kind: ForecastKind,
    pub at: f64,
    /// 리셋이 먼저 와서 낮잠 없이 버팀
    pub safe: bool,
}

#[derive(Clone, Debug)]
pub struct Status {
    pub now: f64,
    pub mood: Mood,
    /// 정확한 값 (0..1). 정확한 값이 없으면 0.
    pub session_pct: f64,
    pub weekly_pct: f64,
    pub session_reset_at: Option<f64>,
    pub weekly_reset_at: Option<f64>,
    pub nap_kind: Option<LimitKind>,
    pub wake_at: Option<f64>,
    /// 0 없음, 1 느림, 2 보통, 3 빠름
    pub hop_speed: u8,
    pub idle_seconds: Option<f64>,
    pub forecast: Option<Forecast>,
    /// 로컬 기록이 하나라도 있는지
    pub has_data: bool,
    /// 서버가 준 정확한 값이 있는지
    pub exact: bool,
}

impl Status {
    pub fn long_idle(&self) -> bool {
        self.idle_seconds.is_none_or(|s| s >= LONG_IDLE)
    }
    pub fn peak_pct(&self) -> f64 {
        self.session_pct.max(self.weekly_pct)
    }
    pub fn session_active(&self) -> bool {
        self.session_reset_at.is_some_and(|r| r > self.now)
    }
}

/// 로컬 기록에서 활동과 한도 메시지만 봅니다.
pub struct Tracker {
    ts: Vec<f64>,
    cum_tokens: Vec<f64>,
    limits: Vec<LimitEvent>,
    last_use: Option<f64>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    pub fn new() -> Self {
        Self { ts: Vec::new(), cum_tokens: vec![0.0], limits: Vec::new(), last_use: None }
    }

    /// events/limits는 시간순이어야 합니다.
    pub fn update(&mut self, events: &[UsageEvent], limits: Vec<LimitEvent>) {
        self.ts = events.iter().map(|e| e.ts).collect();
        self.cum_tokens = Vec::with_capacity(events.len() + 1);
        self.cum_tokens.push(0.0);
        let mut total = 0.0;
        for e in events {
            total += e.tokens as f64;
            self.cum_tokens.push(total);
        }
        self.limits = limits;
        self.last_use = events.last().map(|e| e.ts);
    }

    pub fn last_use(&self) -> Option<f64> {
        self.last_use
    }

    /// 최근 window초 동안 토큰 수
    pub fn tokens_between(&self, start: f64, end: f64) -> f64 {
        let i = self.ts.partition_point(|&t| t < start);
        let j = self.ts.partition_point(|&t| t <= end);
        if j <= i {
            0.0
        } else {
            self.cum_tokens[j] - self.cum_tokens[i]
        }
    }

    /// 정확한 값을 채우기 전의 기본 상태 (활동 + 한도 메시지)
    pub fn status(&self, now: f64) -> Status {
        let idle = self.last_use.map(|t| (now - t).max(0.0));
        let active = idle.is_some_and(|i| i <= ACTIVE_WINDOW);
        // 뛰는 속도: 최근 10분 토큰 흐름 (애니메이션 속도일 뿐, 사용량 계산엔 안 씀)
        let per_min = self.tokens_between(now - 600.0, now) / 10.0;
        let hop_speed = if !active || per_min <= 0.0 {
            0
        } else if per_min < 20_000.0 {
            1
        } else if per_min < 120_000.0 {
            2
        } else {
            3
        };
        let mut st = Status {
            now,
            mood: Mood::Rest,
            session_pct: 0.0,
            weekly_pct: 0.0,
            session_reset_at: None,
            weekly_reset_at: None,
            nap_kind: None,
            wake_at: None,
            hop_speed,
            idle_seconds: idle,
            forecast: None,
            has_data: self.last_use.is_some(),
            exact: false,
        };
        if let Some((kind, wake)) = self.active_nap(now) {
            st.nap_kind = Some(kind);
            st.wake_at = Some(wake);
            match kind {
                LimitKind::Weekly => {
                    st.weekly_pct = 1.0;
                    st.weekly_reset_at = Some(wake);
                }
                _ => {
                    st.session_pct = 1.0;
                    st.session_reset_at = Some(wake);
                }
            }
        }
        st.mood = mood_of(&st, 0.8);
        st
    }

    /// 한도 거절 메시지가 있고, 아직 리셋 전이고, 그 뒤로 다시 쓰지 않았으면 낮잠
    fn active_nap(&self, now: f64) -> Option<(LimitKind, f64)> {
        for ev in self.limits.iter().rev() {
            if ev.ts < now - 8.0 * DAY {
                break;
            }
            let Some(wake) = ev.reset_at else { continue };
            if wake <= now || self.last_use.is_some_and(|t| t > ev.ts + 60.0) {
                continue;
            }
            let kind = if ev.kind == LimitKind::Weekly { LimitKind::Weekly } else { LimitKind::Session };
            return Some((kind, wake));
        }
        None
    }
}

pub fn mood_of(st: &Status, tired_ratio: f64) -> Mood {
    if st.nap_kind.is_some() {
        Mood::Nap
    } else if st.exact && st.peak_pct() >= tired_ratio {
        Mood::Tired
    } else if st.hop_speed > 0 {
        Mood::Hop
    } else {
        Mood::Rest
    }
}

/// 서버가 준 정확한 값을 상태에 채움
pub fn apply_exact(st: &mut Status, b: &BridgeFile, tired_ratio: f64) {
    let now = st.now;
    st.exact = true;
    st.session_pct = b.five_hour.map(|w| w.ratio_at(now)).unwrap_or(0.0);
    st.session_reset_at = b.five_hour.and_then(|w| w.live_reset(now));
    st.weekly_pct = b.seven_day.map(|w| w.ratio_at(now)).unwrap_or(0.0);
    st.weekly_reset_at = b.weekly_reset(now);

    // 한도 거절 메시지로 이미 낮잠이면 그 창은 100%, 리셋은 메시지의 시각
    match (st.nap_kind, st.wake_at) {
        (Some(LimitKind::Weekly), Some(w)) => {
            st.weekly_pct = st.weekly_pct.max(1.0);
            st.weekly_reset_at = Some(w);
        }
        (Some(_), Some(w)) => {
            st.session_pct = st.session_pct.max(1.0);
            st.session_reset_at = Some(w);
        }
        _ => {}
    }
    // 정확한 값이 100%에 닿았고 아직 리셋 전이면 낮잠
    if st.nap_kind.is_none() {
        if let (true, Some(r)) = (st.session_pct >= 1.0, st.session_reset_at) {
            st.nap_kind = Some(LimitKind::Session);
            st.wake_at = Some(r);
        } else if let (true, Some(r)) = (st.weekly_pct >= 1.0, st.weekly_reset_at) {
            st.nap_kind = Some(LimitKind::Weekly);
            st.wake_at = Some(r);
        }
    }
    st.mood = mood_of(st, tired_ratio);
}

/// 정확한 사용률 기록 한 점
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub at: f64,
    pub ratio: f64,
    pub reset: Option<f64>,
}

/// 같은 창 안에서 정확한 값이 오른 속도로 한도 도달 시각 예측 (lookback초 이내 가장 오래된 점 기준)
pub fn forecast_from(history: &[Reading], kind: ForecastKind, lookback: f64, now: f64) -> Option<Forecast> {
    let last = history.last()?;
    let reset = last.reset?;
    if reset <= now || last.ratio >= 1.0 {
        return None;
    }
    let first = history
        .iter()
        .find(|r| r.reset == last.reset && r.at >= last.at - lookback && r.at < last.at - 60.0)?;
    let rate = (last.ratio - first.ratio) / (last.at - first.at);
    if rate <= 0.0 {
        return Some(Forecast { kind, at: reset, safe: true }); // 안 오르는 중 → 리셋까지 버팀
    }
    let at = last.at + (1.0 - last.ratio) / rate;
    Some(Forecast { kind, at, safe: at >= reset })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::BridgeWindow;

    const NOW: f64 = 1_795_000_000.0;
    const H: f64 = HOUR;

    fn ev(ts: f64, tokens: u64) -> UsageEvent {
        UsageEvent { ts, tokens }
    }

    #[test]
    fn activity_moods() {
        let mut t = Tracker::new();
        t.update(&[ev(NOW - 60.0, 50_000)], vec![]);
        let st = t.status(NOW);
        assert_eq!(st.mood, Mood::Hop);
        assert!(!st.exact);
        assert_eq!(st.session_pct, 0.0); // 추정 없음
        t.update(&[ev(NOW - 3.0 * H, 50_000)], vec![]);
        let st = t.status(NOW);
        assert_eq!(st.mood, Mood::Rest);
        assert!(st.long_idle());
    }

    #[test]
    fn limit_message_means_nap_until_reset() {
        let mut t = Tracker::new();
        let lim = LimitEvent { ts: NOW - 600.0, kind: LimitKind::Session, reset_at: Some(NOW + 2.0 * H), text: String::new() };
        t.update(&[ev(NOW - H, 10)], vec![lim.clone()]);
        let st = t.status(NOW);
        assert_eq!(st.mood, Mood::Nap);
        assert_eq!(st.session_pct, 1.0);
        assert_eq!(st.wake_at, Some(NOW + 2.0 * H));
        assert_eq!(t.status(NOW + 2.0 * H + 1.0).mood, Mood::Rest);
        t.update(&[ev(NOW - H, 10), ev(NOW - 10.0, 10)], vec![lim]);
        assert_ne!(t.status(NOW).mood, Mood::Nap); // 다시 쓰는 중 → 풀림
    }

    #[test]
    fn exact_values_and_tired() {
        let t = Tracker::new();
        let mut st = t.status(NOW);
        let b = BridgeFile {
            updated_at: NOW,
            five_hour: Some(BridgeWindow { used_percentage: 60.0, resets_at: Some(NOW + H) }),
            seven_day: Some(BridgeWindow { used_percentage: 85.0, resets_at: Some(NOW + 3.0 * DAY) }),
            weekly_anchor: None,
            models: Vec::new(),
        };
        apply_exact(&mut st, &b, 0.8);
        assert!(st.exact);
        assert!((st.session_pct - 0.6).abs() < 1e-9);
        assert_eq!(st.mood, Mood::Tired);
        let full = BridgeFile { five_hour: Some(BridgeWindow { used_percentage: 100.0, resets_at: Some(NOW + 1800.0) }), ..b };
        let mut st = t.status(NOW);
        apply_exact(&mut st, &full, 0.8);
        assert_eq!(st.mood, Mood::Nap);
        assert_eq!(st.wake_at, Some(NOW + 1800.0));
    }

    #[test]
    fn forecast_from_exact_history() {
        let reset = NOW + 3.0 * H;
        // 30분 동안 30% → 40%: 분당 1/3%, 남은 60% → 180분 뒤 = 리셋과 같음
        let h = [
            Reading { at: NOW - 1800.0, ratio: 0.30, reset: Some(reset) },
            Reading { at: NOW, ratio: 0.40, reset: Some(reset) },
        ];
        let f = forecast_from(&h, ForecastKind::Session, 3600.0, NOW).unwrap();
        assert!((f.at - (NOW + 3.0 * H)).abs() < 1.0);
        let fast = [
            Reading { at: NOW - 1800.0, ratio: 0.30, reset: Some(reset) },
            Reading { at: NOW, ratio: 0.60, reset: Some(reset) },
        ];
        let f = forecast_from(&fast, ForecastKind::Session, 3600.0, NOW).unwrap();
        assert!(!f.safe);
        assert!((f.at - (NOW + 2400.0)).abs() < 1.0);
        // 다른 창(리셋 시각이 다름)의 점은 쓰지 않음
        let other = [Reading { at: NOW - 1800.0, ratio: 0.9, reset: Some(reset - H) }, Reading { at: NOW, ratio: 0.1, reset: Some(reset) }];
        assert!(forecast_from(&other, ForecastKind::Session, 3600.0, NOW).is_none());
    }
}
