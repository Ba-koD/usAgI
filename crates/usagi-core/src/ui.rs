//! 설정 화면(settings.html)에 보낼 계정 정보와, 플랫폼에 상관없는 설정 변경.
//! Windows·macOS 앱이 같이 써서 화면에 보이는 값이 똑같아요.

use crate::accounts::{self, Account, Link};
use crate::config::State;
use crate::engine::{alert_key, alert_rule, Snapshot};
use crate::login::LoginSession;
use crate::{codex, texts};
use serde_json::{json, Value};
use std::sync::OnceLock;

/// 모든 플랫폼이 같이 쓰는 설정 화면 (HTML 한 장). 보안 정책(CSP)을 넣어서 돌려줘요.
///
/// - 스크립트는 이 파일에 들어 있는 것만 실행 (해시로 고정) → 화면에 넣는 글자에 혹시 태그가 섞여도 실행되지 않음
/// - 바깥과의 통신·외부 리소스는 모두 막음 (`default-src 'none'`) → 화면 정보가 밖으로 나갈 길이 없음
pub fn settings_html() -> &'static str {
    static PAGE: OnceLock<String> = OnceLock::new();
    PAGE.get_or_init(|| with_csp(include_str!("settings.html")))
}

fn with_csp(raw: &str) -> String {
    // 브라우저는 줄 끝을 LF로 바꿔 읽으므로 해시도 같은 글자로 계산해야 함
    let html = raw.replace("\r\n", "\n");
    let mut hashes = Vec::new();
    let mut rest = html.as_str();
    while let Some(i) = rest.find("<script>") {
        let body = &rest[i + "<script>".len()..];
        let Some(j) = body.find("</script>") else { break };
        hashes.push(format!("'sha256-{}'", crate::sha256::base64(&crate::sha256::digest(body[..j].as_bytes()))));
        rest = &body[j..];
    }
    let csp = format!(
        "default-src 'none'; script-src {}; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'",
        hashes.join(" ")
    );
    html.replacen("<head>", &format!("<head>\n<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">"), 1)
}

/// 설정 화면이 스스로 다른 곳으로 이동하려 할 때 허용할지. 처음 띄우는 화면(about:blank / data:)만 허용.
pub fn navigation_allowed(url: &str) -> bool {
    url == "about:blank" || url.starts_with("data:text/html")
}

/// 그래프별 기준: 남은 양으로 볼지 (따로 정하지 않았으면 전체 기준)
pub fn meter_left(state: &State, key: &str) -> bool {
    state
        .0
        .get("prefs")
        .and_then(|p| p.get("basis_meters"))
        .and_then(|m| m.get(key))
        .and_then(|v| v.as_str())
        .map(|v| v == "left")
        .unwrap_or_else(|| state.pref_str("basis", "used") == "left")
}

/// 그래프를 위젯(메뉴 막대)에 보일지 (기본 켜짐)
pub fn meter_in_widget(state: &State, key: &str) -> bool {
    state
        .0
        .get("prefs")
        .and_then(|p| p.get("widget_meters"))
        .and_then(|m| m.get(key))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// 설정 화면의 계정 카드 정보
pub fn accounts_json(state: &State, snap: Option<&Snapshot>, login_checking: bool, now: f64) -> Vec<Value> {
    let list = accounts::load(state);
    // 계정 이메일 (Claude는 로그인 상태, Codex는 app-server)
    let email_of = |a: &Account| -> Option<String> {
        let snap = snap?;
        snap.providers
            .iter()
            .find(|p| p.account_id == a.id)
            .and_then(|v| v.email.clone())
            .or_else(|| snap.logins.get(&a.id).and_then(|l| l.email.clone()))
    };
    list.iter()
        .map(|a| {
            let view = snap.and_then(|s| s.providers.iter().find(|p| p.account_id == a.id));
            let email = email_of(a);
            // 같은 공급자에 같은 이메일이 이미 있으면 (브라우저가 기존 계정으로 바로 승인한 경우)
            let dup = email.as_ref().is_some_and(|e| {
                list.iter().any(|b| b.id != a.id && b.provider == a.provider && email_of(b).is_some_and(|x| x.eq_ignore_ascii_case(e)))
            });
            let (link, note) = if a.is_claude() {
                match accounts::link_state(a) {
                    Link::Linked => ("linked", String::new()),
                    Link::NotLinked => ("not_linked", String::new()),
                    Link::Missing(e) => ("missing", e),
                }
            } else {
                ("n/a", String::new())
            };
            let login_info = snap.and_then(|s| s.logins.get(&a.id));
            let login = json!({
                "state": match login_info.and_then(|l| l.logged_in) { Some(true) => "in", Some(false) => "out", None => "unknown" },
                "email": login_info.and_then(|l| l.email.clone()),
                "org": login_info.and_then(|l| l.org.clone()),
                "plan": login_info.and_then(|l| l.plan.clone()),
                "ends": login_info.and_then(|l| l.ends.clone()),
                "error": login_info.and_then(|l| l.error.clone()),
                "checking": login_checking,
            });
            let probe = snap.and_then(|s| s.probes.get(&a.id)).cloned().unwrap_or_default();
            let logged_out = login_info.and_then(|l| l.logged_in) == Some(false);
            let meters: Vec<Value> = view
                .map(|v| {
                    v.meters
                        .iter()
                        .map(|m| {
                            let reset = m.resets_at.filter(|&r| r > now);
                            let hint: String = if !m.exact {
                                if logged_out {
                                    "로그인하면 정확한 값 표시".into()
                                } else if probe.running {
                                    "확인하는 중…".into()
                                } else if let Some(e) = &probe.error {
                                    format!("확인 실패: {e}")
                                } else {
                                    "곧 확인".into()
                                }
                            } else if m.ratio == 0.0 {
                                "리셋됨 · 다음 사용부터 시작".into()
                            } else {
                                "리셋 시각 미수신".into()
                            };
                            let key = alert_key(&a.id, &m.label);
                            let (on, at) = alert_rule(state, &key);
                            json!({
                                "alertKey": key,
                                "alertOn": on,
                                "alertAt": at,
                                "left": meter_left(state, &key),
                                "widget": meter_in_widget(state, &key),
                                "hasOwnBasis": state.0.get("prefs").and_then(|p| p.get("basis_meters")).and_then(|m| m.get(&key)).is_some(),
                                "label": m.label,
                                "ratio": m.ratio,
                                "pct": texts::fmt_pct(m.ratio),
                                "hint": hint,
                                "exact": m.exact,
                                "level": texts::level(m.ratio),
                                "resetAt": reset.map(|r| texts::fmt_clock(r, now)),
                                "remaining": reset.map(|r| texts::fmt_duration(r - now)),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "id": a.id, "provider": a.provider, "dir": a.dir, "label": a.label, "show": a.show,
                "email": email, "plan": view.and_then(|v| v.plan.as_deref()).map(codex::plan_label).filter(|p| !p.is_empty()), "dup": dup,
                "collapsed": state.0.get("prefs").and_then(|p| p.get("collapsed")).and_then(|c| c.as_array())
                    .is_some_and(|c| c.iter().any(|x| x.as_str() == Some(a.id.as_str()))),
                "credits": view.and_then(|v| v.credits.as_ref()).map(|c| json!({ "has": c.has, "unlimited": c.unlimited, "balance": c.balance })),
                "refills": view.and_then(|v| v.refills.as_ref()).map(|r| json!({
                    "available": r.available,
                    "expires": r.expires_at.filter(|&t| t > now).map(|t| texts::fmt_clock(t, now)),
                })),
                "managed": accounts::is_managed(a),
                "name": view.map(|v| v.name.clone()).unwrap_or_else(|| if a.is_claude() { "Claude".into() } else { "Codex".into() }),
                "link": link, "linkNote": note,
                "exact": view.is_some_and(|v| v.meters.iter().any(|m| m.exact)),
                "note": view.and_then(|v| v.note.clone()),
                "forecast": view.and_then(|v| v.forecast.clone()),
                "login": login,
                "probe": { "running": probe.running, "error": probe.error, "triedAt": probe.tried_at.map(|t| texts::fmt_clock(t, now)) },
                "meters": meters,
            })
        })
        .collect()
}

/// 진행 중인 공식 로그인
pub fn login_session_json(session: Option<&LoginSession>) -> Value {
    session
        .map(|s| {
            let v = s.view();
            json!({
                "id": v.account_id, "url": v.url, "acceptsCode": v.accepts_code,
                "done": v.done, "message": v.message, "codeSent": v.code_sent,
            })
        })
        .unwrap_or(Value::Null)
}

/// 그래프·카드 설정 변경 (알림 기준, 그래프별 기준, 위젯 표시, 접기). 처리했으면 true.
pub fn apply_meter_ipc(state: &mut State, cmd: &str, msg: &Value) -> bool {
    let key = msg.get("key").and_then(|k| k.as_str()).unwrap_or("").to_string();
    let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    match cmd {
        "alert" if !key.is_empty() => {
            let alerts = state.0.entry("alerts").or_insert_with(|| json!({}));
            if let Some(map) = alerts.as_object_mut() {
                let rule = map.entry(key).or_insert_with(|| json!({}));
                if let Some(r) = rule.as_object_mut() {
                    if let Some(on) = msg.get("on").and_then(|v| v.as_bool()) {
                        r.insert("on".into(), json!(on));
                    }
                    match msg.get("at") {
                        Some(Value::Array(list)) => {
                            let mut v: Vec<f64> = list.iter().filter_map(|x| x.as_f64()).map(|x| x.clamp(1.0, 99.0)).collect();
                            v.sort_by(|a, b| a.total_cmp(b));
                            v.dedup();
                            r.insert("at".into(), json!(v));
                        }
                        Some(x) => {
                            if let Some(at) = x.as_f64() {
                                r.insert("at".into(), json!([at.clamp(1.0, 99.0)]));
                            }
                        }
                        None => {}
                    }
                }
            }
            state.save();
        }
        "meterBasis" | "meterWidget" => {
            let (field, value) = if cmd == "meterBasis" {
                ("basis_meters", msg.get("value").and_then(|v| v.as_str()).map(|s| json!(s)))
            } else {
                ("widget_meters", msg.get("value").and_then(|v| v.as_bool()).map(|b| json!(b)))
            };
            let prefs = state.0.entry("prefs").or_insert_with(|| json!({}));
            if let Some(m) = prefs.as_object_mut().map(|p| p.entry(field).or_insert_with(|| json!({}))).and_then(|m| m.as_object_mut()) {
                match value {
                    // "default" = 전체 설정을 따라감
                    Some(v) if v != json!("default") => {
                        m.insert(key, v);
                    }
                    _ => {
                        m.remove(&key);
                    }
                }
            }
            state.save();
        }
        "collapse" => {
            let on = msg.get("value").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut list: Vec<String> = state
                .0
                .get("prefs")
                .and_then(|p| p.get("collapsed"))
                .and_then(|c| c.as_array())
                .map(|c| c.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            list.retain(|x| *x != id);
            if on {
                list.push(id);
            }
            state.set_pref("collapsed", json!(list));
        }
        _ => return false,
    }
    true
}

/// 지운 계정의 흔적 정리: 받은 사용량·알림·그래프 설정. usAgI가 만든 프로필이면 로그아웃 후 폴더도.
pub fn forget_account(state: &mut State, a: &Account) {
    let dir = crate::config::config_dir();
    let _ = std::fs::remove_file(dir.join(format!("codex-usage-{}.json", a.id)));
    let _ = std::fs::remove_file(crate::bridge::path(&a.id));
    let prefix = format!("{}:", a.id);
    if let Some(map) = state.0.get_mut("alerts").and_then(|v| v.as_object_mut()) {
        map.retain(|k, _| !k.starts_with(&prefix));
    }
    if let Some(prefs) = state.0.get_mut("prefs").and_then(|p| p.as_object_mut()) {
        for field in ["basis_meters", "widget_meters"] {
            if let Some(map) = prefs.get_mut(field).and_then(|v| v.as_object_mut()) {
                map.retain(|k, _| !k.starts_with(&prefix));
            }
        }
        if let Some(list) = prefs.get_mut("collapsed").and_then(|v| v.as_array_mut()) {
            list.retain(|x| x.as_str() != Some(a.id.as_str()));
        }
    }
    state.save();
    if accounts::is_managed(a) {
        // CLI 로그아웃은 몇 초 걸릴 수 있어 백그라운드에서
        let a = a.clone();
        std::thread::spawn(move || {
            let _ = crate::login::logout(&a);
            let _ = std::fs::remove_dir_all(a.path());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_page_is_locked_down() {
        let raw = include_str!("settings.html");
        // 스크립트는 한 덩어리, 태그에 붙은 이벤트 처리기·바깥 주소 없음 (CSP가 막으므로 넣으면 동작하지 않음)
        assert_eq!(raw.matches("<script").count(), 1);
        assert_eq!(raw.matches("<script>").count(), 1);
        let low = raw.to_lowercase();
        for bad in [" onclick=", " onerror=", " onload=", " oninput=", " onchange=", "javascript:", "src=\"http", "href=\"http", "eval(", "new function("] {
            assert!(!low.contains(bad), "설정 화면에 {bad} 있음");
        }
        let page = settings_html();
        assert!(!page.contains('\r'));
        let i = page.find("<script>").unwrap() + "<script>".len();
        let j = page.find("</script>").unwrap();
        let want = crate::sha256::base64(&crate::sha256::digest(page[i..j].as_bytes()));
        assert!(page.contains(&format!("script-src 'sha256-{want}';")), "CSP 해시와 스크립트 불일치");
        assert!(page.contains("default-src 'none'"));
        // CSP는 스크립트보다 먼저 나와야 함
        assert!(page.find("Content-Security-Policy").unwrap() < i);
    }

    #[test]
    fn only_the_initial_page_may_load() {
        assert!(navigation_allowed("about:blank"));
        assert!(navigation_allowed("data:text/html;charset=utf-8;base64,AAAA"));
        for bad in ["https://example.com/", "http://localhost/", "file:///etc/passwd", "about:srcdoc", "javascript:alert(1)", "data:application/x-msdownload,x"] {
            assert!(!navigation_allowed(bad), "{bad}");
        }
    }
}
