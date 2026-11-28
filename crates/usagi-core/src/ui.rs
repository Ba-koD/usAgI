//! 설정 화면에 보낼 계정 정보와, 플랫폼에 상관없는 설정 변경.
//! Windows·macOS 앱이 같이 써서 화면에 보이는 값이 같음.

use crate::accounts::{self, Account};
use crate::config::State;
use crate::engine::{alert_key, alert_set, lead_in_range, pct_in_range, reset_lead_max, AlertSet, Snapshot};
use crate::login::LoginSession;
use crate::{codex, texts};
use serde_json::{json, Value};

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
            // Codex에서는 같은 이메일의 여러 워크스페이스를 연결할 수 있음.
            let workspace = view.and_then(|v| v.workspace_id.as_deref());
            let login_info = snap.and_then(|s| s.logins.get(&a.id));
            let dup = email.as_ref().is_some_and(|e| {
                list.iter().any(|b| {
                    b.id != a.id && b.provider == a.provider && email_of(b).is_some_and(|x| x.eq_ignore_ascii_case(e))
                        && if a.is_claude() {
                            login_info.zip(snap.and_then(|s| s.logins.get(&b.id)))
                                .is_some_and(|(first, second)| crate::fallback::same_claude_identity(first, second))
                        } else { workspace.is_some_and(|id| {
                            snap.and_then(|s| s.providers.iter().find(|p| p.account_id == b.id))
                                .and_then(|p| p.workspace_id.as_deref()) == Some(id)
                        }) }
                })
            });
            let login = json!({
                "state": match login_info.and_then(|l| l.logged_in) { Some(true) => "in", Some(false) => "out", None => "unknown" },
                "email": login_info.and_then(|l| l.email.clone()),
                "org": login_info.and_then(|l| l.org.clone()),
                "plan": login_info.and_then(|l| l.plan.clone()),
                "ends": login_info.and_then(|l| l.ends.clone()),
                "reconnectSoon": login_info.is_some_and(|l| l.logged_in == Some(true)
                    && l.sign_in_expiry.is_some_and(|expiry| expiry.needs_reconnect(now))),
                "error": login_info.and_then(|l| l.error.clone()),
                // 주 폴더 로그인이 끊겨 대신 쓰는 폴더 (같은 계정)
                "fallback": login_info.and_then(|l| l.fallback.clone()),
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
                                    crate::tr!("로그인하면 정확한 값 표시", "Sign in to see exact values").into()
                                } else if probe.running {
                                    crate::tr!("확인하는 중…", "Checking…").into()
                                } else if let Some(e) = &probe.error {
                                    crate::tr!(format!("확인 실패: {e}"), format!("Check failed: {e}"))
                                } else {
                                    crate::tr!("곧 확인", "Checking soon").into()
                                }
                            } else if m.ratio == 0.0 {
                                crate::tr!("리셋 완료 · 다음 사용부터 다시 집계", "Reset · starts with next use").into()
                            } else {
                                crate::tr!("리셋 시각 정보 없음", "Reset time not received").into()
                            };
                            let key = alert_key(&a.id, &m.label);
                            let alert = alert_set(state, &key);
                            json!({
                                "alertKey": key,
                                "alertOn": alert.on,
                                // 켜 둔 사용량 기준 (막대 위 표시), 꺼 둔 기준, 리셋 임박 시점(리셋 몇 분 전) 켜짐·꺼짐
                                "alertAt": alert.at,
                                "alertRules": crate::alert_rules::rules(state, &key),
                                "alertAtOff": alert.at_off,
                                "before": alert.before,
                                "beforeOff": alert.before_off,
                                "left": meter_left(state, &key),
                                "widget": meter_in_widget(state, &key),
                                "hasOwnBasis": state.0.get("prefs").and_then(|p| p.get("basis_meters")).and_then(|m| m.get(&key)).is_some(),
                                "label": texts::meter_label(&m.label),
                                "ratio": m.ratio,
                                "pct": texts::fmt_pct(m.ratio),
                                "hint": hint,
                                "exact": m.exact,
                                // 다 썼는지 (숫자 대신 "소진"으로 보임)
                                "spent": m.exact && texts::spent(m.ratio),
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
                // Claude 추가 사용량·지출 한도 (보일 글자)
                "extra": view.and_then(|v| v.extra.clone()),
                // 리필권 만료 알림 (ChatGPT 계정): 계정별 설정이 없으면 default
                "refill": {
                    "mode": state.0.get("refill_rules").and_then(|r| r.get(&a.id)).map_or("default", |r| {
                        if r.get("on").and_then(|v| v.as_bool()) == Some(false) { "off" } else { "on" }
                    }),
                    "lead": crate::engine::refill_rule(state, &a.id).1,
                },
                "refills": view.and_then(|v| v.refills.as_ref()).map(|r| json!({
                    "available": r.available,
                    "expires": r.expires_at.filter(|&t| t > now).map(|t| texts::fmt_clock(t, now)),
                })),
                // Claude 쿠폰: 가장 먼저 만료되는 것과 나머지 개수
                "coupons": view.and_then(|v| {
                    let live: Vec<_> = v.coupons.iter().filter(|c| c.expires_at > now).collect();
                    live.first().map(|c| json!({
                        "amount": c.amount_label(),
                        "expires": texts::fmt_clock(c.expires_at, now),
                        "more": live.len() - 1,
                    }))
                }),
                // Claude 사용 크레딧 잔액·자동 충전. 잔액이 없고 자동 충전도 꺼져 있으면 보이지 않음
                "balance": view.and_then(|v| v.balance.as_ref())
                    .filter(|b| b.amount > 0.0 || b.auto_reload == Some(true))
                    .map(|b| json!({ "amount": b.amount_label(), "autoReload": b.auto_reload })),
                "managed": accounts::is_managed(a),
                "name": view.map(|v| v.name.clone()).unwrap_or_else(|| if a.is_claude() { "Claude".into() } else { "Codex".into() }),
                "exact": view.is_some_and(|v| v.meters.iter().any(|m| m.exact)),
                "note": view.and_then(|v| v.note.clone()),
                "forecast": view.and_then(|v| v.forecast.clone()),
                "login": login,
                "probe": {
                    "running": probe.running, "error": probe.error,
                    "triedAt": probe.tried_at.map(|t| texts::fmt_clock(t, now)),
                    "retryAt": probe.retry_at.filter(|t| *t > now).map(|t| texts::fmt_clock(t, now)),
                    "nextAt": probe.next_at.filter(|t| *t > now && !logged_out).map(|t| texts::fmt_clock(t, now)),
                },
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
                "id": v.account_id, "canOpen": v.url.is_some(), "acceptsCode": v.accepts_code,
                "done": v.done, "message": v.message, "codeSent": v.code_sent,
            })
        })
        .unwrap_or(Value::Null)
}

/// 알림 항목 종류: 사용량 기준(%) / 리셋 임박(리셋 몇 분 전)
#[derive(Clone, Copy, PartialEq, Eq)]
enum AlertKind {
    Pct,
    Reset,
}

impl AlertKind {
    /// "kind": "pct"(없으면 이것) | "reset". 그 밖의 값은 None
    fn of(msg: &Value) -> Option<Self> {
        match msg.get("kind").and_then(|k| k.as_str()) {
            None | Some("pct") => Some(Self::Pct),
            Some("reset") => Some(Self::Reset),
            Some(_) => None,
        }
    }

    /// 받는 값 범위: 1~99% / 리셋 1분~해당 그래프 주기 전
    fn valid(self, v: f64, key: &str) -> bool {
        match self {
            Self::Pct => pct_in_range(v),
            Self::Reset => lead_in_range(v, reset_lead_max(key)),
        }
    }

    /// (켜 둔 목록, 꺼 둔 목록)
    fn lists(self, s: &mut AlertSet) -> (&mut Vec<f64>, &mut Vec<f64>) {
        match self {
            Self::Pct => (&mut s.at, &mut s.at_off),
            Self::Reset => (&mut s.before, &mut s.before_off),
        }
    }
}

/// 그래프 알림 설정 저장. 빈 목록은 지우고 ("at"은 예전 형식대로 남김), 규칙의 다른 값은 그대로 둠
fn save_alert_set(state: &mut State, key: &str, s: &AlertSet) {
    let alerts = state.0.entry("alerts").or_insert_with(|| json!({}));
    if !alerts.is_object() {
        *alerts = json!({});
    }
    if let Some(map) = alerts.as_object_mut() {
        let rule = map.entry(key).or_insert_with(|| json!({}));
        if !rule.is_object() {
            *rule = json!({});
        }
        if let Some(r) = rule.as_object_mut() {
            r.insert("on".into(), json!(s.on));
            for (field, list) in [("at", &s.at), ("at_off", &s.at_off), ("before", &s.before), ("before_off", &s.before_off)] {
                let mut v = list.clone();
                v.sort_by(|a, b| a.total_cmp(b));
                v.dedup();
                if v.is_empty() && field != "at" {
                    r.remove(field);
                } else {
                    r.insert(field.into(), json!(v));
                }
            }
        }
    }
}

/// 그래프·카드 설정 변경 (알림 기준, 그래프별 기준, 위젯 표시, 접기). 처리했으면 true.
pub fn apply_meter_ipc(state: &mut State, cmd: &str, msg: &Value) -> bool {
    let key = msg.get("key").and_then(|k| k.as_str()).unwrap_or("").to_string();
    let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    match cmd {
        "alertRule" if key.split_once(':').is_some_and(|(id, label)| !label.is_empty()
            && accounts::load(state).iter().any(|account| account.id == id)) => {
            crate::alert_rules::apply(state, &key, msg);
        }
        // 알림 항목 추가·바꾸기·지우기. kind: "pct"(사용량 %, 기본) | "reset"(리셋 몇 분 전)
        "threshold" if !key.is_empty() => {
            // 다음 스냅숏이 설정 창에 닿기 전에 여러 번 바꿔도 저장된 최신 목록에 적용
            let Some(kind) = AlertKind::of(msg) else { return true };
            let mut set = alert_set(state, &key);
            let old = msg.get("old").and_then(|v| v.as_f64());
            let new = msg.get("new").and_then(|v| v.as_f64()).map(f64::round).filter(|&v| kind.valid(v, &key));
            let (en, off) = kind.lists(&mut set);
            // 이미 있는 값(켜짐·꺼짐 어느 쪽이든)으로는 추가·바꾸기 안 함
            let new = new.filter(|v| !en.contains(v) && !off.contains(v));
            let mut turn_on = false;
            match msg["op"].as_str() {
                Some("add") => {
                    let Some(v) = new else { return true };
                    en.push(v);
                    turn_on = true;
                }
                Some("replace") => {
                    let (Some(v), Some(old)) = (new, old) else { return true };
                    let Some(slot) = en.iter_mut().chain(off.iter_mut()).find(|x| **x == old) else { return true };
                    *slot = v;
                }
                Some("delete") => {
                    let Some(old) = old else { return true };
                    en.retain(|x| *x != old);
                    off.retain(|x| *x != old);
                }
                _ => return true,
            }
            if turn_on {
                set.on = true; // 기준을 새로 넣으면 그래프 알림도 켬
            }
            save_alert_set(state, &key, &set);
        }
        // 알림 항목 하나 켜기·끄기 (지우지 않고 꺼 둔 목록으로 옮김)
        "alertItem" if !key.is_empty() => {
            let kind = AlertKind::of(msg);
            let value = msg.get("value").and_then(|v| v.as_f64());
            let want = msg.get("on").and_then(|v| v.as_bool());
            let (Some(kind), Some(value), Some(want)) = (kind, value, want) else { return true };
            let mut set = alert_set(state, &key);
            let (en, off) = kind.lists(&mut set);
            let (from, to) = if want { (off, en) } else { (en, off) };
            let Some(i) = from.iter().position(|x| *x == value) else { return true };
            from.remove(i);
            to.push(value);
            save_alert_set(state, &key, &set);
        }
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

pub fn clear_usage(a: &Account) {
    let key = a.usage_key();
    let _ = std::fs::remove_file(crate::codex::saved_path(&key));
    let _ = std::fs::remove_file(crate::bridge::path(&key));
    crate::usage_retry::forget(&key);
}

/// 계정 제거와 함께 저장할 알림·그래프 설정 정리.
pub fn forget_account_settings(state: &mut State, a: &Account) {
    let prefix = format!("{}:", a.id);
    for kind in ["login", "login-expiry", "refill", "coupon", "nap"] {
        state.0.remove(&format!("{kind}:{}", a.id));
    }
    state.0.retain(|k, _| !["soon:", "tired:"].iter().any(|p| k.strip_prefix(p).is_some_and(|rest| rest.starts_with(&prefix))));
    if let Some(rules) = state.0.get_mut("refill_rules").and_then(Value::as_object_mut) {
        rules.remove(&a.id);
    }
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
}
