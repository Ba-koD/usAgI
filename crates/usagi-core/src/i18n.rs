//! 화면·알림 언어: 한국어 / 영어. 설정 "locale" = system(기본) | ko | en.
//!
//! 문자열은 쓰는 자리에 두 언어를 같이 둠: `tr!("한국어", "English")`. 고른 쪽 식만 계산하므로 `format!`도 됨.
//! 그래프 이름("5시간", "주간" …)은 알림 기준·위젯 설정의 저장 키라서 한국어 그대로 두고,
//! 화면에 보일 때만 [`crate::texts::meter_label`]로 바꿈.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Ko,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Ko => "ko",
            Lang::En => "en",
        }
    }
}

// 앱에서는 모든 스레드가 같은 언어. 테스트는 병렬로 돌아서 스레드마다 따로 (서로 영향 없게)
#[cfg(not(test))]
static EN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
thread_local! {
    static EN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn is_en() -> bool {
    #[cfg(not(test))]
    return EN.load(std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    return EN.with(|c| c.get());
}

pub fn lang() -> Lang {
    if is_en() {
        Lang::En
    } else {
        Lang::Ko
    }
}

pub fn set(lang: Lang) {
    #[cfg(not(test))]
    EN.store(lang == Lang::En, std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    EN.with(|c| c.set(lang == Lang::En));
}

/// 설정값 → 언어. system(그 밖의 값 포함)이면 OS 표시 언어가 한국어일 때 한국어, 아니면 영어
pub fn resolve(pref: &str) -> Lang {
    match pref {
        "ko" => Lang::Ko,
        "en" => Lang::En,
        _ => system(),
    }
}

/// OS 표시 언어
pub fn system() -> Lang {
    if system_is_korean() {
        Lang::Ko
    } else {
        Lang::En
    }
}

#[cfg(windows)]
fn system_is_korean() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // LANGID의 주 언어 = 아래 10비트, 한국어 = 0x12
    (unsafe { GetUserDefaultUILanguage() } & 0x3ff) == 0x12
}

#[cfg(target_os = "macos")]
fn system_is_korean() -> bool {
    // GUI 앱에는 LANG이 없으므로 시스템 설정의 언어 목록 첫 번째
    std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().trim_matches(|c| c == '"' || c == ',' || c == '(' || c == ')'))
                .find(|l| !l.is_empty())
                .map(|l| l.to_ascii_lowercase().starts_with("ko"))
        })
        .unwrap_or(false)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn system_is_korean() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("ko"))
}

/// 지금 언어의 문자열: `tr!("한국어", "English")`. 고른 쪽 식만 계산 (`format!` 가능)
#[macro_export]
macro_rules! tr {
    ($ko:expr, $en:expr $(,)?) => {
        if $crate::i18n::is_en() {
            $en
        } else {
            $ko
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_choice_wins_and_tr_picks_one_side() {
        assert_eq!(resolve("ko"), Lang::Ko);
        assert_eq!(resolve("en"), Lang::En);
        assert_eq!(resolve("system"), system());
        set(Lang::En);
        assert_eq!(crate::tr!("사용량", "Usage"), "Usage");
        assert_eq!(lang().code(), "en");
        set(Lang::Ko);
        assert_eq!(crate::tr!(String::from("사용량"), String::from("Usage")), "사용량");
    }
}
