//! 버전 비교 (업데이트 확인용). "v0.2.0", "0.2.0", "0.2.0-beta.1" 모두 읽음.

fn parse(v: &str) -> Option<([u64; 3], Option<&str>)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let v = v.split_once('+').map_or(v, |(version, _)| version);
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) if !p.is_empty() && p.split('.').all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }) => (c, Some(p)),
        Some(_) => return None,
        None => (v, None),
    };
    let mut it = core.split('.').map(|p| p.parse::<u64>());
    let major = it.next()?.ok()?;
    let minor = it.next().unwrap_or(Ok(0)).ok()?;
    let patch = it.next().unwrap_or(Ok(0)).ok()?;
    if it.next().is_some() { return None; }
    Some(([major, minor, patch], pre))
}

/// `latest`가 `current`보다 새 버전인지
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse(latest), parse(current)) {
        (Some((a, pre_a)), Some((b, pre_b))) => {
            use std::cmp::Ordering;
            a.cmp(&b).then_with(|| match (pre_a, pre_b) {
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (None, None) => Ordering::Equal,
                (Some(a), Some(b)) => compare_prerelease(a, b),
            }).is_gt()
        }
        _ => false,
    }
}

fn compare_prerelease(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let numeric = |value: &str| value.bytes().all(|b| b.is_ascii_digit());
    for (a, b) in a.split('.').zip(b.split('.')) {
        let cmp = match (numeric(a), numeric(b)) {
            (true, true) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => a.cmp(b),
        };
        if !cmp.is_eq() { return cmp; }
    }
    a.split('.').count().cmp(&b.split('.').count())
}

/// 미리보기(prerelease) 버전인지 ("0.5.0-beta.1"). 읽을 수 없는 글자는 false
pub fn is_prerelease(v: &str) -> bool {
    parse(v).is_some_and(|(_, pre)| pre.is_some())
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
        assert!(is_newer("0.2.0-beta.10", "0.2.0-beta.2"));
        assert!(!is_newer("0.2.0+different-build", "0.2.0"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0"));
        assert!(!is_newer("nightly", "0.1.0"));
        assert_eq!(clean("v1.2.3"), "1.2.3");
    }

    #[test]
    fn detects_prerelease_versions() {
        assert!(is_prerelease("v0.5.0-beta.1"));
        assert!(is_prerelease("0.5.0-rc.1+build"));
        assert!(!is_prerelease("v0.5.0"));
        assert!(!is_prerelease("0.5.0+build-1"));
        assert!(!is_prerelease("nightly"));
        assert!(!is_prerelease("0.5.0-"));
    }
}
