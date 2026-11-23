//! 상태 + 시간 → 지금 그릴 포즈.
//!
//! 포즈 값은 정수로 양자화해서 같은 모양이면 같은 키가 되게 합니다
//! (아이콘 핸들/이미지를 키로 캐시하므로 반복 프레임은 다시 그리지 않음).

use crate::tracker::{Mood, Status};

pub const PEEK_SECONDS: f64 = 5.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Burrow {
    #[default]
    None,
    Sleep,
    Peek,
}

/// 양자화된 포즈. 단위: session/weekly 2.5% 단위(0..=40), lift 단위 길이,
/// squash 0.025 단위, ear 2.5도 단위, paw 1/6, sweat·zzz 1/8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Pose {
    pub session: u8,
    pub weekly: u8,
    pub lift: i8,
    pub squash: i8,
    pub ear_l: i8,
    pub ear_r: i8,
    pub paw: Option<u8>,
    pub sweat: Option<u8>,
    pub zzz: Option<u8>,
    pub burrow: Burrow,
}

impl Pose {
    pub fn gauges(session: f64, weekly: f64) -> Self {
        let q = |r: f64| (r.clamp(0.0, 1.0) * 40.0).round() as u8;
        Pose { session: q(session), weekly: q(weekly), ..Default::default() }
    }
    pub fn session_ratio(&self) -> f32 {
        self.session as f32 / 40.0
    }
    pub fn weekly_ratio(&self) -> f32 {
        self.weekly as f32 / 40.0
    }
}

/// (한 번 뛰는 시간, 착지 후 쉬는 시간)
fn hop_timing(speed: u8) -> (f64, f64) {
    match speed {
        1 => (0.85, 0.9),
        3 => (0.42, 0.0),
        _ => (0.6, 0.25),
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

pub struct Animator {
    rng: Rng,
    mood: Option<Mood>,
    session: f64,
    weekly: f64,
    hop_speed: u8,
    long_idle: bool,
    peek_until: f64,
    hop_origin: f64,
    next_twitch: f64,
    twitch_until: f64,
    twitch_left: bool,
    next_idle_act: f64,
    idle_act: Option<(u8, f64)>,
    next_binky: f64,
    binky_start: f64,
}

impl Animator {
    pub fn new(seed: u64) -> Self {
        Self {
            rng: Rng(seed | 1),
            mood: None,
            session: 0.0,
            weekly: 0.0,
            hop_speed: 0,
            long_idle: false,
            peek_until: 0.0,
            hop_origin: 0.0,
            next_twitch: 0.0,
            twitch_until: 0.0,
            twitch_left: false,
            next_idle_act: 0.0,
            idle_act: None,
            next_binky: 3.0,
            binky_start: f64::MIN,
        }
    }

    /// 게이지(귀)와 기분을 갱신. session/weekly는 0..1 비율.
    pub fn set(&mut self, st: &Status, session: f64, weekly: f64, now: f64) {
        if self.mood == Some(Mood::Nap) && st.mood != Mood::Nap {
            self.peek_until = now + PEEK_SECONDS; // 굴에서 고개 내밀기
        }
        if st.mood == Mood::Hop && self.mood != Some(Mood::Hop) {
            self.hop_origin = now;
        }
        self.mood = Some(st.mood);
        self.session = session;
        self.weekly = weekly;
        self.hop_speed = st.hop_speed;
        self.long_idle = st.long_idle();
    }

    pub fn frame(&mut self, now: f64) -> Pose {
        let base = Pose::gauges(self.session, self.weekly);
        let Some(mood) = self.mood else { return self.rest(base, now) };

        if now < self.peek_until && mood != Mood::Nap {
            return Pose { burrow: Burrow::Peek, ..Default::default() };
        }
        match mood {
            Mood::Nap => Pose {
                burrow: Burrow::Sleep,
                zzz: Some(((now % 2.4) / 2.4 * 8.0) as u8 % 8),
                ..base
            },
            Mood::Tired => {
                // 헥헥: 몸이 살짝 들썩 + 땀방울
                let pant = ((now / 0.45) as i64) % 2 == 0;
                let pose = Pose {
                    squash: if pant { 2 } else { 0 },
                    sweat: Some(((now % 1.6) / 1.6 * 8.0) as u8 % 8),
                    ..base
                };
                if self.hop_speed > 0 {
                    self.hop(pose, now, 1)
                } else {
                    pose
                }
            }
            Mood::Hop => self.hop(base, now, self.hop_speed),
            Mood::Rest => self.rest(base, now),
        }
    }

    /// Time until the next visible change, after evaluating `frame` at `now`.
    pub fn frame_delay(&self, now: f64) -> f64 {
        const FRAME: f64 = 0.066;
        if now < self.peek_until && self.mood != Some(Mood::Nap) {
            return self.peek_until - now;
        }
        match self.mood {
            Some(Mood::Nap) => 0.3 - (now % 2.4) % 0.3,
            Some(Mood::Tired) => FRAME,
            Some(Mood::Hop) => {
                let (hop, pause) = hop_timing(self.hop_speed);
                let phase = (now - self.hop_origin).rem_euclid(hop + pause);
                if phase < hop { FRAME } else { hop + pause - phase }
            }
            Some(Mood::Rest) | None => {
                let binky = now - self.binky_start;
                if (self.idle_act.is_none() && (0.0..0.55).contains(&binky))
                    || (self.long_idle && self.idle_act.is_some())
                {
                    return FRAME;
                }
                let twitch = if now < self.twitch_until { self.twitch_until } else { self.next_twitch };
                let mut next = twitch.min(self.next_binky);
                if self.long_idle {
                    next = next.min(self.next_idle_act);
                }
                (next - now).max(0.0)
            }
        }
    }

    fn hop(&self, pose: Pose, now: f64, speed: u8) -> Pose {
        let (hop, pause) = hop_timing(speed);
        let t = ((now - self.hop_origin).rem_euclid(hop + pause)) / hop;
        if t >= 1.0 {
            return pose; // 착지 후 쉬는 중
        }
        hop_pose(pose, t, 8.0)
    }

    fn rest(&mut self, base: Pose, now: f64) -> Pose {
        // 쉬는 중에도 가끔 제자리에서 작게 깡충 (토끼의 '빙키')
        if now >= self.next_binky {
            self.binky_start = now;
            self.next_binky = now + self.rng.range(7.0, 14.0);
        }
        let t = (now - self.binky_start) / 0.55;
        if (0.0..1.0).contains(&t) && self.idle_act.is_none() {
            return hop_pose(base, t, 5.0);
        }
        self.rest_still(base, now)
    }
}

/// 한 번 뛰는 동작의 t(0..1) 지점 포즈
fn hop_pose(pose: Pose, t: f64, height: f64) -> Pose {
    {
        let (lift, squash, flap) = if t < 0.15 {
            (0.0, 0.10 * (t / 0.15), 0.0) // 웅크리기
        } else if t < 0.85 {
            let u = (t - 0.15) / 0.70; // 공중
            let arc = (std::f64::consts::PI * u).sin();
            (height * arc, if u < 0.5 { -0.05 } else { -0.02 }, 10.0 * arc)
        } else {
            (0.0, 0.10 * (1.0 - (t - 0.85) / 0.15), 0.0) // 착지
        };
        Pose {
            lift: lift.round() as i8,
            squash: (squash / 0.025).round() as i8,
            ear_l: (flap / 2.5).round() as i8,
            ear_r: (flap / 2.5).round() as i8,
            ..pose
        }
    }

}

impl Animator {
    fn rest_still(&mut self, base: Pose, now: f64) -> Pose {
        let mut pose = base;
        // 가끔 귀를 쫑긋
        if now >= self.next_twitch {
            self.twitch_until = now + 0.3;
            self.twitch_left = self.rng.next() < 0.5;
            self.next_twitch = now + self.rng.range(2.5, 6.0);
        }
        if now < self.twitch_until {
            if self.twitch_left {
                pose.ear_l = -5;
            } else {
                pose.ear_r = -5;
            }
        }
        if !self.long_idle {
            return pose;
        }
        // 한동안 안 쓰면 털 고르기 / 기지개
        if self.idle_act.is_none() && now >= self.next_idle_act {
            let kind = if self.rng.next() < 0.66 { 0 } else { 1 };
            self.idle_act = Some((kind, now));
        }
        if let Some((kind, start)) = self.idle_act {
            let t = now - start;
            match kind {
                0 if t < 2.4 => return Pose { paw: Some(((t % 0.8) / 0.8 * 6.0) as u8 % 6), ..base },
                1 if t < 1.6 => {
                    let s = (std::f64::consts::PI * t / 1.6).sin();
                    return Pose { squash: (-0.06 * s / 0.025).round() as i8, ear_l: 2, ear_r: 2, ..base };
                }
                _ => {
                    self.idle_act = None;
                    self.next_idle_act = now + self.rng.range(8.0, 16.0);
                }
            }
        }
        pose
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::Tracker;
    use crate::logs::UsageEvent;

    #[test]
    fn rest_schedules_fast_frames_only_during_motion() {
        let now = 1_795_000_000.0;
        let mut animator = Animator::new(7);
        animator.frame(now);
        assert_eq!(animator.frame_delay(now), 0.066);
        animator.frame(now + 0.6);
        assert!((animator.frame_delay(now + 0.6) - 0.3).abs() < 0.001);
        animator.frame(now + 1.0);
        assert!(animator.frame_delay(now + 1.0) > 1.0);
    }

    #[test]
    fn hop_frames_are_finite() {
        let now = 1_795_000_000.0;
        let mut t = Tracker::new();
        let events: Vec<UsageEvent> = (0..30).map(|i| UsageEvent { ts: now - 600.0 + i as f64 * 20.0, tokens: 40_000 }).collect();
        t.update(&events, vec![]);
        let st = t.status(now);
        let mut a = Animator::new(7);
        a.set(&st, 0.4, 0.2, now);
        let keys: std::collections::HashSet<Pose> = (0..600).map(|i| a.frame(now + i as f64 * 0.066)).collect();
        assert!(keys.len() > 3 && keys.len() < 80, "{}", keys.len());
    }
}
