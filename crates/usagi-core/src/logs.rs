//! Claude Code 로컬 로그(~/.claude/projects/**/*.jsonl) 증분 리더.
//!
//! Claude Code가 남기는 대화 로그에서 두 가지만 봅니다 (사용량 추정은 하지 않음).
//! - assistant 메시지의 토큰 흐름 → [`UsageEvent`] (지금 쓰는 중인지, 토끼가 뛰는 속도)
//! - "You've hit your session limit · resets 9:40pm (Asia/Seoul)" → [`LimitEvent`]
//! 파일마다 읽은 위치를 기억해 두고 새로 붙은 줄만 읽습니다.

use crate::time::{self, DAY};
use memchr::memmem;
use serde::Deserialize;
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub struct UsageEvent {
    pub ts: f64,
    /// 입력 + 출력 + 캐시 쓰기 토큰 (캐시 읽기 제외) — 활동량 표시용
    pub tokens: u64,
}

#[derive(Debug, Default, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LimitKind {
    Session,
    Weekly,
    Other,
}

#[derive(Clone, Debug)]
pub struct LimitEvent {
    pub ts: f64,
    pub kind: LimitKind,
    pub reset_at: Option<f64>,
    pub text: String,
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

pub fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(env) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        for p in std::env::split_paths(&env) {
            if !p.as_os_str().is_empty() {
                roots.push(p.join("projects"));
            }
        }
    }
    if let Some(home) = home_dir() {
        roots.push(home.join(".claude").join("projects"));
        roots.push(home.join(".config").join("claude").join("projects"));
    }
    let mut seen = Vec::new();
    roots.retain(|r| {
        let key = r.to_string_lossy().to_lowercase();
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    roots
}

/// 디렉터리 아래 확장자가 맞는 파일을 재귀로 모읍니다 (glob 크레이트 없이).
/// 기록 폴더 전체를 훑는 간격(초). 그 사이(3초마다)에는 최근에 바뀐 파일만 확인해요.
/// 기록 파일이 수천 개여도 평소엔 몇 개만 들여다보게 됨. 새로 생긴 파일은 다음 전체 훑기 때 잡혀요.
pub const FULL_SCAN_SECS: f64 = 15.0;
/// 이 시간 안에 바뀐 파일은 "최근" (매번 확인)
pub const HOT_SECS: f64 = 15.0 * 60.0;

pub fn mtime_secs(meta: &std::fs::Metadata) -> f64 {
    meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub fn walk_files(root: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            walk_files(&path, ext, out);
        } else if ft.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
            out.push(path);
        }
    }
}

// ------------------------------------------------------------------ 한도 메시지

pub fn limit_kind(text: &str) -> LimitKind {
    let low = text.to_lowercase();
    if low.contains("week") {
        LimitKind::Weekly
    } else if low.contains("session") || low.contains("5-hour") || low.contains("5 hour")
        || low.contains("usage limit reached")
    {
        LimitKind::Session
    } else {
        LimitKind::Other
    }
}

const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn parse_clock(tok: &str, next: Option<&str>) -> Option<(u32, u32, bool)> {
    // "9:40pm", "9pm", "21:00", "9" + 다음 토큰 "pm" → (시, 분, 다음 토큰 소비 여부)
    let (body, mut ap) = if let Some(b) = tok.strip_suffix("am") {
        (b, Some(false))
    } else if let Some(b) = tok.strip_suffix("pm") {
        (b, Some(true))
    } else {
        (tok, None)
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (body.parse::<u32>().ok()?, 0),
    };
    let mut used_next = false;
    if ap.is_none() {
        match next {
            Some("am") => (ap, used_next) = (Some(false), true),
            Some("pm") => (ap, used_next) = (Some(true), true),
            _ => {}
        }
    }
    let mut hour = h;
    match ap {
        Some(true) if hour != 12 => hour += 12,
        Some(false) if hour == 12 => hour = 0,
        _ => {}
    }
    (hour <= 23 && m <= 59).then_some((hour, m, used_next))
}

/// 한도 메시지에서 리셋 시각(epoch)을 뽑습니다.
pub fn parse_reset(text: &str, event_ts: f64) -> Option<f64> {
    let low = text.to_lowercase();
    if let Some(pos) = low.find("limit reached|") {
        let digits: String = low[pos + 14..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.len() >= 9 {
            return digits.parse::<f64>().ok();
        }
    }
    let start = low.find("reset")?;
    let rest = &low[start + 5..];
    let rest = rest.strip_prefix('s').unwrap_or(rest);
    // 괄호 속 시간대 이름은 원문 대소문자로
    let orig_rest = text.get(text.len().saturating_sub(rest.len())..).unwrap_or(rest);
    let (body, tz) = match (orig_rest.find('('), orig_rest.find(')')) {
        (Some(a), Some(b)) if b > a => (
            rest.get(..a).unwrap_or(rest),
            orig_rest.get(a + 1..b).and_then(time::tz_named),
        ),
        _ => (rest, None),
    };
    let tz = tz.unwrap_or_else(jiff::tz::TimeZone::system);

    let tokens: Vec<String> = body
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(|t| t.trim_matches('.').to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let (mut wd, mut month, mut day, mut clock) = (None, None, None, None);
    let mut i = 0;
    while i < tokens.len() && clock.is_none() {
        let t = tokens[i].as_str();
        if t == "at" || t == "on" {
        } else if let Some(w) = WEEKDAYS.iter().position(|w| t.starts_with(w)) {
            wd = Some(w as u32);
        } else if let Some(m) = MONTHS.iter().position(|m| t.starts_with(m)) {
            month = Some(m as u32 + 1);
            if let Some(d) = tokens.get(i + 1).and_then(|d| d.parse::<u32>().ok()) {
                day = Some(d);
                i += 1;
            }
        } else if let Some((h, m, used)) = parse_clock(t, tokens.get(i + 1).map(|s| s.as_str())) {
            clock = Some((h, m));
            if used {
                i += 1;
            }
        } else if clock.is_none() && wd.is_none() && month.is_none() {
            break; // 시각 앞에 모르는 단어
        }
        i += 1;
    }
    let (hour, minute) = clock?;
    let base = time::parts_in(event_ts, &tz);

    if let (Some(mo), Some(d)) = (month, day) {
        let mut ts = time::to_ts_in(base.year, mo, d, hour, minute, &tz)?;
        if ts < event_ts - 180.0 * DAY {
            ts = time::to_ts_in(base.year + 1, mo, d, hour, minute, &tz)?;
        }
        return Some(ts);
    }
    let mut add = 0i64;
    if let Some(w) = wd {
        add = ((w + 7 - base.weekday) % 7) as i64;
    }
    let (y, mo, d) = time::add_days(base.year, base.month, base.day, add);
    let mut ts = time::to_ts_in(y, mo, d, hour, minute, &tz)?;
    if ts <= event_ts {
        let (y, mo, d) = time::add_days(y, mo, d, if wd.is_some() { 7 } else { 1 });
        ts = time::to_ts_in(y, mo, d, hour, minute, &tz)?;
    }
    Some(ts)
}

// ------------------------------------------------------------------ 리더

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type", borrow, default)]
    ty: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(rename = "requestId", borrow, default)]
    request_id: Option<&'a RawValue>,
    #[serde(borrow, default)]
    error: Option<&'a RawValue>,
    #[serde(rename = "isApiErrorMessage", borrow, default)]
    is_api_error: Option<&'a RawValue>,
    #[serde(borrow, default)]
    uuid: Option<&'a RawValue>,
    #[serde(borrow, default)]
    message: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow, default)]
    id: Option<&'a RawValue>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(borrow, default)]
    content: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<serde_json::Value>),
}

fn fnv(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for &b in *p {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

const CHUNK: usize = 8 * 1024 * 1024;

pub struct LogReader {
    pub roots: Vec<PathBuf>,
    pub history_days: f64,
    offsets: HashMap<PathBuf, u64>,
    index: HashMap<u64, usize>,
    events: Vec<UsageEvent>,
    limits: HashMap<(i64, LimitKind), LimitEvent>,
    sorted: Vec<UsageEvent>,
    dirty: bool,
    /// 마지막으로 폴더 전체를 훑은 시각
    last_full: f64,
    /// 최근에 바뀐 파일 (전체 훑기 사이에 확인)
    hot: Vec<PathBuf>,
}

impl LogReader {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            history_days: 60.0,
            offsets: HashMap::new(),
            index: HashMap::new(),
            events: Vec::new(),
            limits: HashMap::new(),
            sorted: Vec::new(),
            dirty: true,
            last_full: 0.0,
            hot: Vec::new(),
        }
    }

    /// 새 줄을 읽습니다. 새 데이터가 있었으면 true.
    pub fn poll(&mut self) -> bool {
        let now = time::now();
        let cutoff = now - self.history_days * DAY;
        // 폴더 전체는 가끔만 훑고, 그 사이엔 최근에 바뀐 파일만 확인
        let full = now - self.last_full >= FULL_SCAN_SECS;
        let files = if full {
            let mut files = Vec::new();
            for root in &self.roots {
                walk_files(root, "jsonl", &mut files);
            }
            self.last_full = now;
            self.hot.clear();
            files
        } else {
            self.hot.clone()
        };
        let mut changed = false;
        for path in files {
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let size = meta.len();
            let mtime = mtime_secs(&meta);
            if full && mtime >= now - HOT_SECS {
                self.hot.push(path.clone());
            }
            let known = self.offsets.get(&path).copied();
            if known.is_none() {
                if mtime < cutoff {
                    self.offsets.insert(path, size); // 오래된 파일은 건너뜀
                    continue;
                }
            }
            let mut offset = known.unwrap_or(0);
            if size < offset {
                offset = 0; // 파일이 다시 쓰였음
            }
            if size == offset {
                continue;
            }
            let new_offset = self.read_from(&path, offset, cutoff);
            if new_offset != offset {
                self.offsets.insert(path, new_offset);
                changed = true;
            }
        }
        if changed {
            self.dirty = true;
            self.prune(cutoff);
        }
        changed
    }

    pub fn usage_events(&mut self) -> &[UsageEvent] {
        if self.dirty {
            self.sorted = self.events.clone();
            self.sorted.sort_by(|a, b| a.ts.total_cmp(&b.ts));
            self.dirty = false;
        }
        &self.sorted
    }

    pub fn limit_events(&self) -> Vec<LimitEvent> {
        let mut v: Vec<LimitEvent> = self.limits.values().cloned().collect();
        v.sort_by(|a, b| a.ts.total_cmp(&b.ts));
        v
    }

    fn read_from(&mut self, path: &Path, offset: u64, cutoff: f64) -> u64 {
        let Ok(mut f) = File::open(path) else { return offset };
        if f.seek(SeekFrom::Start(offset)).is_err() {
            return offset;
        }
        let mut pos = offset; // 완전한 줄까지만 소비한 위치
        let mut pending: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = match f.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            pending.extend_from_slice(&buf[..n]);
            let Some(last_nl) = memchr::memrchr(b'\n', &pending) else { continue };
            for line in pending[..last_nl].split(|&b| b == b'\n') {
                if !line.is_empty() {
                    self.handle_line(line, cutoff);
                }
            }
            pos += (last_nl + 1) as u64;
            pending.drain(..=last_nl);
        }
        pos // 덜 쓰인 마지막 줄은 다음 poll에서 다시 읽음
    }

    fn handle_line(&mut self, line: &[u8], cutoff: f64) {
        if memmem::find(line, b"\"assistant\"").is_none() {
            return;
        }
        let has_usage = memmem::find(line, b"\"usage\"").is_some();
        let maybe_limit = memmem::find(line, b"limit").is_some()
            && (memmem::find(line, b"\"rate_limit\"").is_some()
                || memmem::find(line, b"\"isApiErrorMessage\"").is_some());
        if !has_usage && !maybe_limit {
            return;
        }
        let Ok(obj) = serde_json::from_slice::<Line>(line) else { return };
        if obj.ty.as_deref() != Some("assistant") {
            return;
        }
        let Some(ts) = obj.timestamp.as_deref().and_then(time::parse_iso) else { return };
        if ts < cutoff {
            return;
        }
        let Some(raw_msg) = obj.message else { return };
        let Ok(msg) = serde_json::from_str::<Message>(raw_msg.get()) else { return };

        if maybe_limit {
            let is_rate_limit = obj.error.is_some_and(|e| e.get() == "\"rate_limit\"");
            let is_api_error = obj.is_api_error.is_some_and(|e| e.get() == "true");
            if is_rate_limit || is_api_error {
                if let Some(text) = msg.content.and_then(content_text) {
                    let low = text.to_lowercase();
                    let looks_like_limit =
                        (low.contains("limit") && low.contains("reset")) || low.contains("usage limit reached");
                    if is_rate_limit || looks_like_limit {
                        let kind = limit_kind(&text);
                        let ev = LimitEvent { ts, kind, reset_at: parse_reset(&text, ts), text: text.trim().to_string() };
                        self.limits.insert((ts.round() as i64, kind), ev);
                        return;
                    }
                }
            }
        }

        let Some(usage) = msg.usage else { return };
        let tokens = usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0) + usage.cache_creation_input_tokens.unwrap_or(0);
        if tokens == 0 {
            return;
        }
        let key = match (msg.id, obj.request_id) {
            (None, None) => fnv(&[obj.uuid.map(|u| u.get().as_bytes()).unwrap_or(b"")]),
            (id, rid) => fnv(&[
                id.map(|v| v.get().as_bytes()).unwrap_or(b""),
                rid.map(|v| v.get().as_bytes()).unwrap_or(b""),
            ]),
        };
        // 같은 메시지가 여러 줄/여러 파일에 반복 기록되므로 마지막 값으로 덮어씀
        match self.index.get(&key) {
            Some(&i) => {
                let e = &mut self.events[i];
                e.tokens = tokens;
                e.ts = e.ts.min(ts);
            }
            None => {
                self.index.insert(key, self.events.len());
                self.events.push(UsageEvent { ts, tokens });
            }
        }
    }

    fn prune(&mut self, cutoff: f64) {
        if self.events.len() < 50_000 {
            return;
        }
        let mut keep = Vec::with_capacity(self.events.len());
        let mut remap = HashMap::new();
        for (key, &i) in &self.index {
            let e = self.events[i];
            if e.ts >= cutoff {
                remap.insert(*key, keep.len());
                keep.push(e);
            }
        }
        self.events = keep;
        self.index = remap;
        self.limits.retain(|_, l| l.ts >= cutoff);
    }
}

fn content_text(raw: &RawValue) -> Option<String> {
    match serde_json::from_str::<Content>(raw.get()).ok()? {
        Content::Text(s) => Some(s),
        Content::Blocks(blocks) => blocks
            .iter()
            .find_map(|b| b.get("text").and_then(|t| t.as_str()).map(str::to_string)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn seoul(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> f64 {
        time::to_ts_in(y, mo, d, h, mi, &time::tz_named("Asia/Seoul").unwrap()).unwrap()
    }

    #[test]
    fn reset_session_message() {
        let sent = seoul(2026, 6, 18, 17, 10);
        let text = "You've hit your session limit · resets 9:40pm (Asia/Seoul)";
        assert_eq!(limit_kind(text), LimitKind::Session);
        assert_eq!(parse_reset(text, sent), Some(seoul(2026, 6, 18, 21, 40)));
    }

    #[test]
    fn reset_rolls_and_dates() {
        let sent = seoul(2026, 6, 18, 23, 10);
        assert_eq!(parse_reset("limit · resets 2am (Asia/Seoul)", sent), Some(seoul(2026, 6, 19, 2, 0)));
        let sent = seoul(2026, 11, 21, 10, 0);
        let weekly = "You've hit your weekly limit · resets Nov 25, 9am (Asia/Seoul)";
        assert_eq!(limit_kind(weekly), LimitKind::Weekly);
        assert_eq!(parse_reset(weekly, sent), Some(seoul(2026, 11, 25, 9, 0)));
        assert_eq!(parse_reset("Claude AI usage limit reached|1750000000", 0.0), Some(1_750_000_000.0));
    }

    fn line(ts: &str, id: &str, out: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","requestId":"r-{id}","message":{{"id":"{id}","model":"claude-opus-5-5","content":[{{"type":"text","text":"hi"}}],"usage":{{"input_tokens":10,"output_tokens":{out}}}}}}}"#
        ) + "\n"
    }

    #[test]
    fn incremental_dedup_partial_and_limits() {
        let dir = std::env::temp_dir().join(format!("usagi-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proj")).unwrap();
        let p = dir.join("proj").join("s.jsonl");
        let now = time::now();
        let iso = {
            let secs = now as i64;
            let (y, m, d) = time::civil_from_days(secs.div_euclid(86400));
            let r = secs.rem_euclid(86400);
            format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z", r / 3600, r % 3600 / 60, r % 60)
        };
        std::fs::write(&p, line(&iso, "a", 100) + &line(&iso, "a", 100)).unwrap();
        let mut r = LogReader::new(vec![dir.clone()]);
        assert!(r.poll());
        assert_eq!(r.usage_events().len(), 1);

        let partial = line(&iso, "b", 200);
        let (head, tail) = partial.split_at(partial.len() - 30);
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(head.as_bytes()).unwrap();
        r.poll();
        assert_eq!(r.usage_events().len(), 1);
        f.write_all(tail.as_bytes()).unwrap();
        assert!(r.poll());
        assert_eq!(r.usage_events().len(), 2);
        assert!(!r.poll());

        let err = format!(
            r#"{{"type":"assistant","error":"rate_limit","isApiErrorMessage":true,"timestamp":"{iso}","message":{{"id":"x","model":"<synthetic>","content":[{{"type":"text","text":"You've hit your session limit · resets 9:40pm (Asia/Seoul)"}}],"usage":{{"input_tokens":0,"output_tokens":0}}}}}}"#
        ) + "\n";
        f.write_all(err.as_bytes()).unwrap();
        assert!(r.poll());
        let limits = r.limit_events();
        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].kind, LimitKind::Session);
        assert!(limits[0].reset_at.is_some());
        assert_eq!(r.usage_events().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
