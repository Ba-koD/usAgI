//! 버전 비교 (업데이트 확인용). "v0.2.0", "0.2.0", "0.2.0-beta.1" 모두 읽음.

/// (주, 부, 수, 정식 여부) — 정식이 같은 번호의 미리 보기(-beta)보다 새것
fn parse(v: &str) -> Option<(u64, u64, u64, bool)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, !p.is_empty()),
        None => (v, false),
    };
    let mut it = core.split('.').map(|p| p.parse::<u64>());
    let major = it.next()?.ok()?;
    let minor = it.next().unwrap_or(Ok(0)).ok()?;
    let patch = it.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch, !pre))
}

/// `latest`가 `current`보다 새 버전인지
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse(latest), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// 태그에서 앞의 v를 뗀 표시용 버전
pub fn clean(tag: &str) -> String {
    tag.trim().trim_start_matches(['v', 'V']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(is_newer("0.2.0", "0.2.0-beta.1"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0"));
        assert!(!is_newer("nightly", "0.1.0"));
        assert_eq!(clean("v1.2.3"), "1.2.3");
    }
}
