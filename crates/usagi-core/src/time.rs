//! 시간 도우미: 현재 시각, ISO 8601 파싱(빠른 수동 파서), 현지 시간 변환(jiff).

use jiff::{civil, tz::TimeZone, Timestamp};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MINUTE: f64 = 60.0;
pub const HOUR: f64 = 3600.0;
pub const DAY: f64 = 86400.0;

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// 외부에서 받은 시각(epoch 초)으로 믿을 수 있는 가장 늦은 값: 2100-01-01
pub const MAX_TIMESTAMP: f64 = 4_102_444_800.0;

/// 공급자·CLI·로그가 준 시각을 받아들일 때: 숫자가 아니거나 무한이면 버리고, 나머지는 1970~2100으로 맞춤.
/// 터무니없는 값이 남은 시간 계산(`Duration`)까지 가서 앱이 멈추지 않게
pub fn timestamp(at: f64) -> Option<f64> {
    at.is_finite().then(|| at.clamp(0.0, MAX_TIMESTAMP))
}

/// 초 → `Duration`. 음수·NaN은 0, 너무 크면 `Duration::MAX` (`Duration::from_secs_f64`는 이런 값에서 패닉)
pub fn duration_secs(seconds: f64) -> std::time::Duration {
    std::time::Duration::try_from_secs_f64(seconds).unwrap_or(if seconds > 0.0 {
        std::time::Duration::MAX
    } else {
        std::time::Duration::ZERO
    })
}

fn system_tz() -> &'static TimeZone {
    static TZ: OnceLock<TimeZone> = OnceLock::new();
    TZ.get_or_init(TimeZone::system)
}

pub fn tz_named(name: &str) -> Option<TimeZone> {
    TimeZone::get(name.trim()).ok()
}

/// 현지 시각 구성 요소. weekday: 월=0 … 일=6
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub weekday: u32,
}

impl Parts {
    pub fn day_number(&self) -> i64 {
        days_from_civil(self.year as i64, self.month as i64, self.day as i64)
    }
}

pub fn local(ts: f64) -> Parts {
    parts_in(ts, system_tz())
}

pub fn parts_in(ts: f64, tz: &TimeZone) -> Parts {
    let t = Timestamp::from_second(ts.floor() as i64).unwrap_or(Timestamp::UNIX_EPOCH);
    let z = t.to_zoned(tz.clone());
    Parts {
        year: z.year() as i32,
        month: z.month() as u32,
        day: z.day() as u32,
        hour: z.hour() as u32,
        minute: z.minute() as u32,
        weekday: z.weekday().to_monday_zero_offset() as u32,
    }
}

/// 현지 날짜/시각 → epoch 초 (DST 틈은 jiff 기본 규칙으로 처리)
pub fn local_to_ts(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> Option<f64> {
    to_ts_in(y, mo, d, h, mi, system_tz())
}

pub fn to_ts_in(y: i32, mo: u32, d: u32, h: u32, mi: u32, tz: &TimeZone) -> Option<f64> {
    let dt = civil::DateTime::new(y as i16, mo as i8, d as i8, h as i8, mi as i8, 0, 0).ok()?;
    let z = dt.to_zoned(tz.clone()).ok()?;
    Some(z.timestamp().as_second() as f64)
}

/// (년, 월, 일)에 n일 더하기
pub fn add_days(y: i32, mo: u32, d: u32, n: i64) -> (i32, u32, u32) {
    civil_from_days(days_from_civil(y as i64, mo as i64, d as i64) + n)
}

pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((if m <= 2 { y + 1 } else { y }) as i32, m as u32, d as u32)
}

/// "2026-11-21T14:50:59.435Z" / "+09:00" 오프셋 → epoch 초. 로그 수만 줄을 읽으므로 수동 파서.
pub fn parse_iso(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let mut v = 0i64;
        for &c in &b[from..to] {
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + (c - b'0') as i64;
        }
        Some(v)
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut i = 19;
    let mut frac = 0.0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let mut scale = 0.1;
        while i < b.len() && b[i].is_ascii_digit() {
            frac += (b[i] - b'0') as f64 * scale;
            scale /= 10.0;
            i += 1;
        }
    }
    let mut offset = 0i64;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') && b.len() >= i + 6 {
        let sign = if b[i] == b'+' { 1 } else { -1 };
        offset = sign * (num(i + 1, i + 3)? * 3600 + num(i + 4, i + 6)? * 60);
    }
    let days = days_from_civil(y, mo, d);
    Some((days * 86_400 + h * 3600 + mi * 60 + sec - offset) as f64 + frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(parse_iso("2026-06-18T08:10:56.000Z"), Some(1_781_770_256.0));
        assert_eq!(parse_iso("2026-06-18T17:10:56+09:00"), Some(1_781_770_256.0));
    }

    #[test]
    fn untrusted_times_never_reach_a_panicking_duration() {
        assert_eq!(timestamp(f64::NAN), None);
        assert_eq!(timestamp(f64::INFINITY), None);
        assert_eq!(timestamp(1e300), Some(MAX_TIMESTAMP));
        assert_eq!(timestamp(-5.0), Some(0.0));
        assert_eq!(timestamp(1_795_000_000.0), Some(1_795_000_000.0));
        use std::time::Duration;
        assert_eq!(duration_secs(f64::NAN), Duration::ZERO);
        assert_eq!(duration_secs(-1.0), Duration::ZERO);
        assert_eq!(duration_secs(1e300), Duration::MAX);
        assert_eq!(duration_secs(f64::INFINITY), Duration::MAX);
        assert_eq!(duration_secs(1.5), Duration::from_millis(1500));
    }

    #[test]
    fn civil_roundtrip() {
        for z in [-1000i64, 0, 19_000, 20_779] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y as i64, m as i64, d as i64), z);
        }
        assert_eq!(add_days(2026, 12, 31, 1), (2027, 1, 1));
    }
}
