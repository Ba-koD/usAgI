//! 변경 내용: 빌드할 때 넣은 CHANGELOG.md에서 한 버전의 구역을 꺼내고, 마크다운을 화면에 보일 줄로 나눔.
//!
//! 설정 → 정보의 "변경 내용"과 새 버전 안내(GitHub 릴리스 노트, 같은 형식)에 씀.
//! 화면은 글자 꾸밈을 섞어 그리지 못하므로 제목·목록·문단만 구분하고, 굵게·코드·링크 표시는 벗겨 냄.

/// 저장소의 CHANGELOG.md (빌드할 때 포함)
const TEXT: &str = include_str!("../../../CHANGELOG.md");

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// `#`~`######` 제목
    Heading,
    /// `- ` / `* ` 목록 항목
    Bullet,
    /// 그 밖의 문단
    Text,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub kind: Kind,
    /// 목록 들여쓰기 단계 (0부터)
    pub level: u8,
    pub text: String,
}

/// 이 빌드에 들어 있는 한 버전의 변경 내용 (`## [버전]` 다음부터 다음 `## [` 전까지). 없으면 None
pub fn section(version: &str) -> Option<&'static str> {
    section_of(TEXT, version)
}

fn section_of<'a>(text: &'a str, version: &str) -> Option<&'a str> {
    let head = format!("## [{}]", version.trim_start_matches(['v', 'V']));
    let mut start = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with("## [") {
            if let Some(s) = start {
                return Some(text[s..offset].trim());
            }
            if line.starts_with(&head) {
                start = Some(offset + line.len());
            }
        }
        offset += line.len();
    }
    start.map(|s| text[s..].trim()).filter(|s| !s.is_empty())
}

/// 마크다운을 화면 줄로: 제목, 목록(들여쓰기 2칸마다 한 단계), 문단. 빈 줄은 버림
pub fn lines(markdown: &str) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    for raw in markdown.lines() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let heading = line.trim_start_matches('#');
        if heading.len() < line.len() && heading.starts_with(' ') {
            out.push(Line { kind: Kind::Heading, level: 0, text: plain(heading) });
        } else if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            out.push(Line { kind: Kind::Bullet, level: (indent / 2).min(4) as u8, text: plain(item) });
        } else if indent > 0 && out.last().is_some_and(|l| l.kind == Kind::Bullet) {
            // 들여 쓴 이어지는 줄은 앞 항목에 붙임
            let last = out.last_mut().expect("checked");
            last.text.push(' ');
            last.text.push_str(&plain(line));
        } else {
            out.push(Line { kind: Kind::Text, level: 0, text: plain(line.trim_start_matches('>').trim()) });
        }
    }
    out
}

/// 글자 꾸밈 표시를 벗김: `**굵게**`, `` `코드` ``, `[글자](주소)` → 글자
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s.trim();
    while let Some(open) = rest.find('[') {
        // [글자](주소) 모양일 때만 글자만 남김
        let Some(close) = rest[open..].find("](").map(|i| open + i) else { break };
        let Some(end) = rest[close..].find(')').map(|i| close + i) else { break };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..close]);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "").replace('`', "")
}

/// 긴 글을 줄 단위로 잘라 `max` 글자 안쪽으로 (화면에 보낼 새 버전 안내용)
pub fn clip(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for line in text.lines() {
        let n = line.chars().count() + 1;
        if used + n > max {
            break;
        }
        out.push_str(line);
        out.push('\n');
        used += n;
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# 변경 내용\n\n## [0.2.0] - 2026-11-23\n\n### 알림\n- 리셋 전 알림 추가\n- `settings.json` **권한** 유지 ([문서](https://example.com))\n  - 하위 항목\n\n문단 한 줄\n\n## [0.1.0] - 2026-11-22\n\n### 첫 버전\n- 위젯\n";

    #[test]
    fn picks_one_version_section() {
        let s = section_of(SAMPLE, "0.2.0").unwrap();
        assert!(s.starts_with("### 알림") && s.ends_with("문단 한 줄"), "{s}");
        assert!(!s.contains("첫 버전"));
        assert_eq!(section_of(SAMPLE, "v0.1.0").unwrap(), "### 첫 버전\n- 위젯");
        assert_eq!(section_of(SAMPLE, "9.9.9"), None);
        // 0.2.0을 찾을 때 0.2.0-beta 같은 다른 구역과 섞이지 않음
        assert_eq!(section_of("## [0.2.0-beta] - x\n- a\n## [0.2.0] - y\n- b\n", "0.2.0").unwrap(), "- b");
    }

    #[test]
    fn splits_markdown_into_lines() {
        let l = lines(section_of(SAMPLE, "0.2.0").unwrap());
        assert_eq!(l[0], Line { kind: Kind::Heading, level: 0, text: "알림".into() });
        assert_eq!(l[1], Line { kind: Kind::Bullet, level: 0, text: "리셋 전 알림 추가".into() });
        assert_eq!(l[2].text, "settings.json 권한 유지 (문서)", "굵게·코드·링크 표시 제거");
        assert_eq!((l[3].kind, l[3].level, l[3].text.as_str()), (Kind::Bullet, 1, "하위 항목"));
        assert_eq!(l[4], Line { kind: Kind::Text, level: 0, text: "문단 한 줄".into() });
        assert_eq!(l.len(), 5, "빈 줄은 버림");
        // 대괄호만 있고 링크가 아니면 그대로
        assert_eq!(plain("[0.2.0] 메모"), "[0.2.0] 메모");
    }

    #[test]
    fn clips_at_line_boundaries() {
        assert_eq!(clip("가나다\n라마바\n사아자", 9), "가나다\n라마바");
        assert_eq!(clip("짧음", 100), "짧음");
    }

    #[test]
    fn bundled_changelog_parses() {
        // 이 빌드의 버전 구역이 있으면 줄로 나눠져야 함 (버전을 먼저 올린 개발 중에는 없을 수 있음)
        if let Some(s) = section(crate::VERSION) {
            assert!(lines(s).iter().any(|l| l.kind == Kind::Bullet));
        }
    }
}
