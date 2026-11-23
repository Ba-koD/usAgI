use super::{Account, AlertItem, CompoundRule, LoginInfo, Meter, NoteLine, Preferences, RuleCondition, RuleDetail, SettingsView, UpdateInfo};
use serde_json::Value;
use slint::{Model, ModelRc, SharedString, VecModel};
use usagi_core::tr;
use usagi_core::alert_rules::{Comparison, Logic, Metric, Rule};

pub const METRICS: [Metric; 3] = [Metric::Time, Metric::Left, Metric::Used];
pub const COMPARISONS: [Comparison; 5] = [Comparison::Lt, Comparison::Le, Comparison::Eq, Comparison::Gt, Comparison::Ge];

pub const INTERVALS: [u32; 5] = [1, 3, 5, 10, 15];

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

fn flag(v: &Value, key: &str) -> bool {
    v[key].as_bool().unwrap_or(false)
}

fn list(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}

// Keep repeaters and their focused inputs alive across periodic snapshots.
fn rows<T: Clone + PartialEq + 'static>(current: ModelRc<T>, values: Vec<T>) -> ModelRc<T> {
    let Some(model) = current.as_any().downcast_ref::<VecModel<T>>() else {
        return ModelRc::new(VecModel::from(values));
    };
    while model.row_count() > values.len() {
        model.remove(model.row_count() - 1);
    }
    for (i, value) in values.into_iter().enumerate() {
        match model.row_data(i) {
            None => model.push(value),
            Some(old) if old != value => model.set_row_data(i, value),
            _ => {}
        }
    }
    current
}

fn compound(rule: Rule, previous: Option<CompoundRule>) -> CompoundRule {
    let logic = if rule.logic == Logic::And { "AND" } else { "OR" };
    let separator = format!("  {logic}  ");
    let (old_conditions, old_details) = previous.map(|r| (r.conditions, r.details)).unwrap_or_default();
    let conditions = rule.conditions.iter().map(|c| RuleCondition {
        metric: METRICS.iter().position(|m| *m == c.metric).unwrap() as i32,
        op: COMPARISONS.iter().position(|op| *op == c.op).unwrap() as i32,
        value: c.value as i32,
    }).collect();
    let details = rule.conditions.iter().map(|c| RuleDetail {
        metric: c.metric.label().into(), value: c.description().into(),
    }).collect();
    let full = rule.conditions.iter().map(|c| c.description()).collect::<Vec<_>>().join(&separator);
    let compact = rule.conditions.iter().map(|c| format!("{}{}", c.op.symbol(), c.amount())).collect::<Vec<_>>().join(&separator);
    let count = rule.conditions.len();
    let summary = if count == 1 { compact.clone() }
        else { tr!(format!("조건 {count}개 {logic}"), format!("{count} conditions · {logic}")) };
    let accessible = rule.conditions.iter().map(|c| format!("{} {}", c.metric.label(), c.description())).collect::<Vec<_>>().join(&separator);
    CompoundRule {
        id: rule.id as i32, logic: (rule.logic == Logic::Or) as i32,
        conditions: rows(old_conditions, conditions), details: rows(old_details, details),
        full: full.into(), compact: compact.into(), summary: summary.into(), accessible_text: accessible.into(),
    }
}

fn meter(v: &Value, previous: Option<Meter>) -> Meter {
    let key = text(v, "alertKey");
    let reset_lead_max = usagi_core::engine::reset_lead_max(key);
    let left = flag(v, "left");
    let ratio = v["ratio"].as_f64().unwrap_or_default().clamp(0.0, 1.0);
    // 남은 시간과 리셋 시각은 따로 (좁은 그래프에서는 두 줄로)
    let (detail, reset) = match v["resetAt"].as_str() {
        Some(reset) => (
            tr!(
                format!("{} 뒤 리셋", text(v, "remaining")),
                format!("Resets in {}", text(v, "remaining"))
            ),
            reset.to_owned(),
        ),
        None => (text(v, "hint").to_owned(), String::new()),
    };
    let nums = |key: &str| -> Vec<i32> { list(&v[key]).iter().filter_map(Value::as_f64).map(|x| x.round() as i32).collect() };
    let thresholds = nums("alertAt");
    // 알림 칩: 사용량(켜짐·꺼짐) → 리셋 전(켜짐·꺼짐), 각각 값 순서
    let mut items: Vec<AlertItem> = Vec::new();
    let pct_text = |at: i32| format!("{}%", if left { 100 - at } else { at });
    let lead_text = |m: i32| {
        let lead = usagi_core::texts::fmt_lead(m as f64);
        tr!(format!("{lead} 전"), format!("{lead} before"))
    };
    let mut pct: Vec<(i32, bool)> = nums("alertAt").into_iter().map(|x| (x, true)).chain(nums("alertAtOff").into_iter().map(|x| (x, false))).collect();
    // 남은 양 기준이면 남은 양 순서(큰 값 먼저 = 사용량 작은 값 먼저)로
    pct.sort_by_key(|&(x, _)| x);
    let mut lead: Vec<(i32, bool)> = nums("before").into_iter().map(|x| (x, true)).chain(nums("beforeOff").into_iter().map(|x| (x, false))).collect();
    lead.sort_by_key(|&(x, _)| x);
    for (x, on) in &pct {
        items.push(AlertItem { kind: 0, value: *x, on: *on, text: pct_text(*x).into() });
    }
    for (x, on) in &lead {
        items.push(AlertItem { kind: 1, value: *x, on: *on, text: lead_text(*x).into() });
    }
    let taken_pct: Vec<i32> = pct.iter().map(|&(x, _)| x).collect();
    let taken_reset: Vec<i32> = lead.iter().map(|&(x, _)| x).collect();
    let (old_thresholds, old_alerts, old_pct, old_reset, old_rules) = match previous {
        Some(m) => (m.thresholds, m.alerts, m.taken_pct, m.taken_reset, m.compound),
        None => Default::default(),
    };
    let rules = list(&v["alertRules"]).iter().filter_map(|raw| serde_json::from_value::<Rule>(raw.clone()).ok())
        .enumerate().map(|(i, rule)| {
            let previous = old_rules.row_data(i).filter(|r| r.id == rule.id as i32);
            compound(rule, previous)
        }).collect();
    Meter {
        key: key.into(),
        label: text(v, "label").into(),
        exact: flag(v, "exact"),
        spent: flag(v, "spent"),
        percent: ((if left { 1.0 - ratio } else { ratio }) * 100.0).round() as i32,
        level: v["level"].as_u64().unwrap_or_default().min(2) as i32,
        left,
        detail: detail.into(),
        reset: reset.into(),
        widget: flag(v, "widget"),
        own_basis: flag(v, "hasOwnBasis"),
        alert_on: flag(v, "alertOn"),
        reset_lead_max: reset_lead_max as i32,
        reset_lead_label: usagi_core::texts::fmt_lead(f64::from(reset_lead_max)).into(),
        thresholds: rows(old_thresholds, thresholds),
        alerts: rows(old_alerts, items),
        compound: rows(old_rules, rules),
        taken_pct: rows(old_pct, taken_pct),
        taken_reset: rows(old_reset, taken_reset),
    }
}

fn refill_noun(n: u64) -> &'static str {
    if n == 1 {
        "refill"
    } else {
        "refills"
    }
}

fn account(v: &Value, previous: Option<Account>) -> Account {
    let login = &v["login"];
    let probe = &v["probe"];
    let label = text(v, "label");
    let email = v["email"].as_str().unwrap_or_else(|| text(login, "email"));
    let title = [label, email, text(v, "name")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    let status = text(login, "state");
    let login_status = match status {
        "in" if email.is_empty() || label.is_empty() => tr!("로그인됨", "Logged in"),
        "in" => "",
        "out" => tr!("로그인 필요", "Login required"),
        _ => tr!("로그인 상태 확인 중", "Checking login status"),
    };
    let details = [
        login_status,
        if label.is_empty() { "" } else { email },
        text(login, "org"),
        if flag(login, "reconnectSoon") { "" } else { text(login, "ends") },
        text(login, "error"),
        if text(login, "fallback").is_empty() { "" } else { tr!("대체 로그인 폴더 사용 중", "Using a backup sign-in folder") },
        if flag(v, "dup") {
            tr!("다른 카드와 동일한 계정", "Same account as another card")
        } else {
            ""
        },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let footer = [text(v, "forecast"), text(v, "note")]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let retry = if flag(probe, "running") {
        tr!("확인 중…", "Checking…").to_owned()
    } else if let Some(at) = probe["retryAt"].as_str().or_else(|| probe["nextAt"].as_str()) {
        tr!(format!("다음 확인 {at}"), format!("Next check {at}"))
    } else if let Some(at) = probe["triedAt"].as_str() {
        tr!(format!("마지막 확인 {at}"), format!("Last checked {at}"))
    } else {
        String::new()
    };
    let refresh_status = [text(probe, "error"), &retry].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
    // 크레딧·리필권은 카드 머리줄의 작은 표시로
    let credits = &v["credits"];
    let credits = if flag(credits, "unlimited") {
        tr!("크레딧 무제한", "Unlimited credits").to_string()
    } else {
        // 남은 크레딧이 없으면 표시하지 않음
        match credits["balance"].as_f64() {
            Some(balance) if balance > 0.0 => tr!(format!("크레딧 {balance}"), format!("{balance} credits")),
            _ => String::new(),
        }
    };
    // 만료는 날짜만 ("12월 13일(일) 오전 7시 22분" → "12월 13일")
    let refills = match (v["refills"]["available"].as_u64(), v["refills"]["expires"].as_str()) {
        (Some(n), Some(expires)) => {
            let date = expires.split('(').next().unwrap_or(expires).trim();
            tr!(
                format!("리필권 {n}장 · {date}까지"),
                format!("{n} {} · until {date}", refill_noun(n))
            )
        }
        (Some(n), None) => tr!(format!("리필권 {n}장"), format!("{n} {}", refill_noun(n))),
        _ => String::new(),
    };
    let old_meters = previous.map(|a| a.meters).unwrap_or_default();
    let meters = list(&v["meters"])
        .iter()
        .enumerate()
        .map(|(i, m)| meter(m, old_meters.row_data(i)))
        .collect();
    Account {
        id: text(v, "id").into(),
        provider: text(v, "provider").into(),
        label: label.into(),
        title: title.into(),
        plan: v["plan"]
            .as_str()
            .unwrap_or_else(|| text(login, "plan"))
            .into(),
        status: status.into(),
        // Codex 크레딧이 없으면 Claude 추가 사용량·지출 한도를 같은 자리에
        credits: if credits.is_empty() { text(v, "extra").to_string() } else { credits }.into(),
        refill_mode: match text(&v["refill"], "mode") {
            "on" | "off" => text(&v["refill"], "mode"),
            _ => "default",
        }
        .into(),
        refill_lead: v["refill"]["lead"].as_f64().unwrap_or(1440.0).round() as i32,
        refills: refills.into(),
        // 대체 폴더를 쓰는 중이면 두 폴더 모두
        folder: match text(login, "fallback") {
            "" => text(v, "dir").to_string(),
            fb => tr!(
                format!("{} (로그인 만료) · 사용 중: {fb}", text(v, "dir")),
                format!("{} (signed out) · in use: {fb}", text(v, "dir"))
            ),
        }
        .into(),
        details: details.into(),
        footer: footer.into(),
        refresh_status: refresh_status.into(),
        refreshing: flag(probe, "running"),
        managed: flag(v, "managed"),
        reconnect_soon: flag(login, "reconnectSoon"),
        sign_in_ends: text(login, "ends").into(),
        collapsed: flag(v, "collapsed"),
        show_widget: flag(v, "show"),
        meters: rows(old_meters, meters),
    }
}

/// 마크다운(변경 내용) → 화면 줄
fn note_lines(markdown: &str) -> Vec<NoteLine> {
    use usagi_core::changelog::{lines, Kind};
    lines(markdown)
        .into_iter()
        .map(|l| NoteLine {
            kind: match l.kind {
                Kind::Heading => 0,
                Kind::Bullet => 1,
                Kind::Text => 2,
            },
            level: l.level as i32,
            text: l.text.into(),
        })
        .collect()
}

fn update(v: &Value) -> UpdateInfo {
    let latest = text(v, "latest");
    let status = text(v, "status");
    let managed = flag(v, "brew");
    let mut description = match status {
        "checking" => tr!("업데이트 확인 중…", "Checking for updates…").into(),
        "latest" => tr!(
            format!("최신 버전 · {}", text(v, "checkedAt")),
            format!("Up to date · {}", text(v, "checkedAt"))
        ),
        "available" => tr!(format!("새 버전 v{latest}"), format!("New version v{latest}")),
        "downloading" => tr!(format!("v{latest} 다운로드 중…"), format!("Downloading v{latest}…")),
        "ready" => tr!(format!("v{latest} 다운로드 완료"), format!("v{latest} downloaded")),
        "error" => text(v, "error").into(),
        _ => tr!("업데이트 확인 전", "Not checked yet").into(),
    };
    if let Some(hint) = v["managedHint"].as_str() {
        description.push_str(" · ");
        description.push_str(hint);
    }
    let install = match status {
        "ready" if !managed => tr!("지금 다시 시작", "Restart now"),
        "available" | "error" if !managed && !latest.is_empty() => tr!("업데이트 설치", "Install update"),
        _ => "",
    };
    UpdateInfo {
        status: description.into(),
        install: install.into(),
        managed,
        busy: matches!(status, "checking" | "downloading"),
        failed: status == "error",
    }
}

pub fn apply(view: &SettingsView, state: &Value) {
    let prefs = &state["prefs"];
    let mut monitor_ids: Vec<SharedString> = vec!["primary".into(), "all".into()];
    let mut monitor_names: Vec<SharedString> = vec![
        tr!("주 모니터", "Primary monitor").into(),
        tr!("모든 모니터", "All monitors").into(),
    ];
    for monitor in list(&state["monitors"]) {
        monitor_ids.push(text(monitor, "id").into());
        monitor_names.push(text(monitor, "label").into());
    }
    let monitor_index = monitor_ids
        .iter()
        .position(|id| id == text(prefs, "monitor"))
        .unwrap_or(0) as i32;
    view.set_monitor_ids(rows(view.get_monitor_ids(), monitor_ids));
    view.set_monitors(rows(view.get_monitors(), monitor_names));
    view.set_prefs(Preferences {
        widget: flag(prefs, "widget"),
        monitor_index,
        side: text(prefs, "side").into(),
        mode: text(prefs, "mode").into(),
        theme: match text(prefs, "theme") {
            "light" | "dark" => text(prefs, "theme"),
            _ => "system",
        }
        .into(),
        bar_px: prefs["barPx"].as_f64().or(prefs["barDefault"].as_f64()).unwrap_or(77.0).round() as i32,
        bar_min: prefs["barMin"].as_f64().unwrap_or(60.0).round() as i32,
        bar_max: prefs["barMax"].as_f64().unwrap_or(240.0).round() as i32,
        bar_default: prefs["barDefault"].as_f64().unwrap_or(77.0).round() as i32,
        bars: prefs["bars"].as_bool().unwrap_or(true),
        locale: match text(prefs, "locale") {
            "ko" | "en" => text(prefs, "locale"),
            _ => "system",
        }
        .into(),
        logo: match text(prefs, "logo") {
            "white" | "ink" => text(prefs, "logo"),
            _ => "auto",
        }
        .into(),
        basis: text(prefs, "basis").into(),
        refresh: INTERVALS
            .iter()
            .position(|&m| Some(m as f64) == prefs["refresh"].as_f64())
            .unwrap_or(2) as i32,
        notify: flag(prefs, "notify"),
        nap: flag(prefs, "napNotify"),
        login: prefs["loginNotify"].as_bool().unwrap_or(true),
        login_warning_days: prefs["loginWarningDays"].as_i64().unwrap_or(3) as i32,
        refill: flag(prefs, "refillNotify"),
        refill_lead: prefs["refillLead"].as_f64().unwrap_or(1440.0).round() as i32,
        urgent: flag(prefs, "urgent"),
        urgent_supported: flag(prefs, "urgentSupported"),
        urgent_status: match prefs["urgentAllowed"].as_bool() {
            Some(true) => tr!("Windows 중요 알림 허용됨", "Windows important notifications allowed"),
            Some(false) => tr!("Windows 중요 알림 꺼짐", "Windows important notifications off"),
            None => "",
        }
        .into(),
        autostart: flag(prefs, "autostart"),
        auto_update: flag(prefs, "autoUpdate"),
    });
    let old = view.get_accounts();
    let accounts = list(&state["accounts"])
        .iter()
        .enumerate()
        .map(|(i, v)| account(v, old.row_data(i).filter(|a| a.id == text(v, "id"))))
        .collect();
    view.set_accounts(rows(old, accounts));
    let inactive_profiles = list(&state["inactiveProfiles"]).iter().filter_map(Value::as_str).map(SharedString::from).collect();
    view.set_inactive_profiles(rows(view.get_inactive_profiles(), inactive_profiles));
    let login = &state["loginSession"];
    view.set_login(LoginInfo {
        id: text(login, "id").into(),
        provider: if text(login, "id").starts_with("codex") { "codex" } else { "claude" }.into(),
        message: match (login["done"].as_bool(), text(login, "message")) {
            (Some(true), "") => tr!("로그인 완료 · 계정 정보 불러오는 중", "Logged in · Loading account info"),
            (Some(false), "") => tr!("로그인 실패", "Login failed"),
            (None, "") if flag(login, "codeSent") => tr!("코드 전송 완료 · 확인 중", "Code sent · Verifying"),
            (None, "") => tr!("브라우저에서 로그인을 승인하면 연결 완료", "Approve the login in your browser to connect the account"),
            (_, message) => message,
        }
        .into(),
        can_open: flag(login, "canOpen") && login["done"].is_null(),
        accepts_code: flag(login, "acceptsCode")
            && !flag(login, "codeSent")
            && login["done"].is_null(),
        done: login["done"].is_boolean(),
        failed: login["done"].as_bool() == Some(false),
    });
    // macOS: Slint는 창을 투명하게 만들어서 제목 표시줄 뒤로 다른 창이 비쳐 보임 → 불투명 창으로 (한 번만)
    #[cfg(target_os = "macos")]
    {
        use slint::winit_030::WinitWindowAccessor;
        use slint::ComponentHandle;
        use std::sync::atomic::{AtomicBool, Ordering};
        static OPAQUE: AtomicBool = AtomicBool::new(false);
        if !OPAQUE.load(Ordering::Relaxed) && view.window().with_winit_window(|w| w.set_transparent(false)).is_some() {
            OPAQUE.store(true, Ordering::Relaxed);
        }
    }
    // 창 제목 표시줄도 같은 테마로 (Windows는 Slint 색 설정이 제목 표시줄까지 가지 않음). 바뀔 때만.
    {
        use slint::winit_030::{winit::window::Theme as WinitTheme, WinitWindowAccessor};
        use slint::ComponentHandle;
        use std::sync::atomic::{AtomicU8, Ordering};
        static APPLIED: AtomicU8 = AtomicU8::new(u8::MAX);
        let (code, theme) = match text(prefs, "theme") {
            "light" => (1, Some(WinitTheme::Light)),
            "dark" => (2, Some(WinitTheme::Dark)),
            _ => (0, None),
        };
        if APPLIED.load(Ordering::Relaxed) != code && view.window().with_winit_window(|w| w.set_theme(theme)).is_some() {
            APPLIED.store(code, Ordering::Relaxed);
        }
    }
    // 작업 표시줄 단추 아이콘: 로고 색 설정을 따르고, 자동이면 작업 표시줄이 밝을 때 진한 토끼 (Windows 모드)
    #[cfg(windows)]
    if let Some(light) = match text(prefs, "logo") {
        "ink" => Some(true),
        "white" => Some(false),
        _ => state["taskbarLight"].as_bool(),
    } {
        use slint::winit_030::winit::{platform::windows::WindowExtWindows, window::Icon};
        use slint::winit_030::WinitWindowAccessor;
        use slint::ComponentHandle;
        use std::sync::atomic::{AtomicU8, Ordering};
        use usagi_core::rabbit::{icon_rgba, ICON_INK, ICON_WHITE};
        static TASKBAR: AtomicU8 = AtomicU8::new(u8::MAX);
        let code = light as u8;
        if TASKBAR.load(Ordering::Relaxed) != code {
            let n = 64;
            if let Ok(icon) = Icon::from_rgba(icon_rgba(n, if light { ICON_INK } else { ICON_WHITE }), n, n) {
                if view.window().with_winit_window(|w| w.set_taskbar_icon(Some(icon))).is_some() {
                    TASKBAR.store(code, Ordering::Relaxed);
                }
            }
        }
    }
    apply_widget_preview(view, &state["widgetPreview"]);
    view.set_update(update(&state["update"]));
    view.set_update_notes(rows(view.get_update_notes(), note_lines(text(&state["update"], "notes"))));
    // 이 버전의 변경 내용 (빌드에 들어 있는 CHANGELOG). 한 번만 채움
    if view.get_version_notes().row_count() == 0 {
        if let Some(section) = usagi_core::changelog::section(usagi_core::VERSION) {
            view.set_version_notes(ModelRc::new(VecModel::from(note_lines(section))));
        }
    }
    view.set_error(text(state, "error").into());
    view.set_install_help(text(state, "installHelp").into());
    view.set_windows(text(state, "platform") == "windows");
    view.set_version(text(state, "version").into());
    view.set_folder(text(state, "dataDir").into());
    view.set_portable(flag(state, "portable"));
}

/// Windows 호스트가 그려 보낸 위젯 그림. {keep}이면 그대로 두고, 없으면 비움.
pub fn apply_widget_preview(view: &SettingsView, preview: &Value) {
    match preview {
        Value::Object(p) if p.contains_key("rgba") => {
            let (w, h) = (p["w"].as_u64().unwrap_or(0) as u32, p["h"].as_u64().unwrap_or(0) as u32);
            let scale = p["scale"].as_f64().filter(|s| *s > 0.0).unwrap_or(1.0) as f32;
            let bytes = p["rgba"].as_str().and_then(usagi_core::b64::decode).unwrap_or_default();
            let ok = w > 0 && h > 0 && bytes.len() == (w * h * 4) as usize;
            if ok {
                let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&bytes, w, h);
                view.set_widget_preview(slint::Image::from_rgba8(buffer));
                view.set_preview_width(w as f32 / scale);
                view.set_preview_height(h as f32 / scale);
                view.set_preview_light(p["light"].as_bool().unwrap_or(false));
            }
            view.set_has_preview(ok);
        }
        Value::Object(_) => {}
        _ => {
            if view.get_has_preview() {
                view.set_widget_preview(slint::Image::default());
                view.set_has_preview(false);
            }
        }
    }
}
