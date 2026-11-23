//! Native settings view shared by the desktop hosts.
mod view_state;

use serde_json::{json, Value};
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::io::{self, BufRead, Write};

/// 첫 상태를 받았을 때 언어를 한 번은 꼭 적용 (Slint가 시스템 언어로 먼저 골랐을 수 있음)
static LANG_SET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
slint::include_modules!();

fn send(message: Value) {
    let _ = writeln!(io::stdout().lock(), "{message}");
}

fn wire_actions(view: &SettingsView) {
    view.on_preview_visibility(|visible| send(json!({"cmd":"widgetPreview","visible":visible})));
    let actions = view.global::<Actions>();
    actions.on_has_threshold(|values, threshold| values.iter().any(|v| v == threshold));
    actions.on_drag_payload(|id| {
        let mut data = slint::DataTransfer::default();
        data.set_user_data(std::rc::Rc::new(id));
        data
    });
    actions.on_dragged_account(|data| {
        data.user_data()
            .and_then(|v| v.downcast_ref::<slint::SharedString>().cloned())
            .unwrap_or_default()
    });
    actions.on_command(|cmd| send(json!({"cmd":cmd.as_str()})));
    actions.on_reset_page(|scope| send(json!({"cmd":"resetPrefs","scope":scope.as_str()})));
    actions.on_widget_bar_preview(|px, active| send(json!({"cmd":"widgetBarLength","value":active.then_some(px)})));
    let weak = view.as_weak();
    actions.on_pref(move |key, value| {
        let key = key.as_str();
        let value = match key {
            "refresh" => value
                .parse::<usize>()
                .ok()
                .and_then(|i| view_state::INTERVALS.get(i))
                .map(|m| json!(m)),
            "monitor" => weak
                .upgrade()
                .and_then(|v| v.get_monitor_ids().row_data(value.parse().ok()?))
                .map(|id| json!(id.as_str())),
            "side" | "mode" | "basis" | "theme" | "logo" | "locale" => Some(json!(value.as_str())),
            // 막대 길이(px). "default"면 기본으로
            "barPx" => Some(value.parse::<f64>().ok().filter(|v| v.is_finite()).map_or(Value::Null, |v| json!(v))),
            "refillLead" => value.parse::<f64>().ok().filter(|v| v.is_finite()).map(|v| json!(v)),
            _ => value.parse::<bool>().ok().map(Value::Bool),
        };
        if let Some(value) = value {
            send(json!({"cmd":"pref","key":key,"value":value}));
        }
    });
    let weak = view.as_weak();
    actions.on_account(move |cmd, id, value| {
        let (cmd, id, value) = (cmd.as_str(), id.as_str(), value.as_str());
        let message = match cmd {
            "addNew" => json!({"cmd":cmd,"provider":id,"preferExisting":value=="preferExisting"}),
            "openUsage" => json!({"cmd":cmd,"provider":id}),
            "loginCode" => json!({"cmd":cmd,"code":value}),
            "label" => json!({"cmd":"account","id":id,"field":cmd,"value":value}),
            "show" => json!({"cmd":"account","id":id,"field":cmd,"value":value=="true"}),
            "collapse" => json!({"cmd":cmd,"id":id,"value":value=="true"}),
            "up" | "down" | "move" => {
                let Some(view) = weak.upgrade() else { return };
                let mut ids: Vec<_> = view
                    .get_accounts()
                    .iter()
                    .map(|a| a.id.to_string())
                    .collect();
                let Some(from) = ids.iter().position(|a| a == id) else {
                    return;
                };
                let to = match cmd {
                    "up" => from.saturating_sub(1),
                    "down" => (from + 1).min(ids.len() - 1),
                    _ => match value.parse::<usize>() {
                        Ok(i) if i < ids.len() => i,
                        _ => return,
                    },
                };
                let moved = ids.remove(from);
                ids.insert(to, moved);
                json!({"cmd":"reorder","ids":ids})
            }
            _ => json!({"cmd":cmd,"id":id}),
        };
        send(message);
    });
    actions.on_refill(|id, mode, lead| send(json!({"cmd":"refillRule","id":id.as_str(),"mode":mode.as_str(),"lead":lead})));
    wire_rule_editor(view);
    // 그래프 알림 하나 (코어 cmd: threshold / alertItem, kind: pct | reset)
    actions.on_alert(|op, key, kind, old, new| {
        let kind = if kind == 1 { "reset" } else { "pct" };
        let key = key.as_str();
        let message = match op.as_str() {
            "add" => json!({"cmd":"threshold","key":key,"kind":kind,"op":"add","new":new}),
            "replace" => json!({"cmd":"threshold","key":key,"kind":kind,"op":"replace","old":old,"new":new}),
            "delete" => json!({"cmd":"threshold","key":key,"kind":kind,"op":"delete","old":old}),
            "toggle" => json!({"cmd":"alertItem","key":key,"kind":kind,"value":old,"on":new != 0}),
            _ => return,
        };
        send(message);
    });
    actions.on_meter(|op, key, old, new| {
        let key = key.as_str();
        let message = match op.as_str() {
            "basis" => json!({"cmd":"meterBasis","key":key,"value":match old {1=>"used",2=>"left",_=>"default"}}),
            "widget" => json!({"cmd":"meterWidget","key":key,"value":old!=0}),
            "enabled" => json!({"cmd":"alert","key":key,"on":old!=0}),
            _ => json!({"cmd":"threshold","key":key,"op":op.as_str(),"old":old,"new":new}),
        };
        send(message);
    });
}

fn rule_condition(item: RuleCondition) -> Option<usagi_core::alert_rules::Condition> {
    Some(usagi_core::alert_rules::Condition {
        metric: *view_state::METRICS.get(item.metric as usize)?,
        op: *view_state::COMPARISONS.get(item.op as usize)?,
        value: u32::try_from(item.value).ok()?,
    })
}

fn update_rule_summary(actions: &Actions<'_>) {
    let separator = if actions.get_rule_logic() == 0 {
        usagi_core::tr!(" 그리고 ", " and ")
    } else {
        usagi_core::tr!(" 또는 ", " or ")
    };
    let terms = actions.get_rule_conditions().iter().filter_map(rule_condition).map(|term| {
        format!("{} {}", term.metric.label(), term.description())
    }).collect::<Vec<_>>();
    actions.set_rule_summary(terms.join(separator).into());
}

fn wire_rule_editor(view: &SettingsView) {
    use usagi_core::alert_rules::Logic;
    let actions = view.global::<Actions>();
    let weak = view.as_weak();
    actions.on_open_rule(move |key, label, maximum, rule| {
        let Some(view) = weak.upgrade() else { return };
        let actions = view.global::<Actions>();
        let mut draft = rule.conditions.iter().collect::<Vec<_>>();
        if draft.is_empty() {
            draft = vec![
                RuleCondition { metric: 0, op: 1, value: maximum.min(60) },
                RuleCondition { metric: 1, op: 1, value: 20 },
            ];
        }
        actions.set_rule_meter(label);
        actions.set_rule_max(maximum);
        actions.set_rule_id(rule.id);
        actions.set_rule_logic(rule.logic);
        actions.set_rule_conditions(ModelRc::new(VecModel::from(draft)));
        actions.set_rule_key(key);
        update_rule_summary(&actions);
    });
    let weak = view.as_weak();
    actions.on_rule_logic_changed(move || {
        if let Some(view) = weak.upgrade() { update_rule_summary(&view.global::<Actions>()); }
    });
    let weak = view.as_weak();
    actions.on_rule_row(move |field, index, value| {
        let Some(view) = weak.upgrade() else { return };
        let actions = view.global::<Actions>();
        let model = actions.get_rule_conditions();
        let Some(rows) = model.as_any().downcast_ref::<VecModel<RuleCondition>>() else { return };
        if field == "add" {
            let metric = (0..3).find(|metric| rows.iter().all(|condition| condition.metric != *metric)).unwrap_or(1);
            let (op, value) = match metric {
                0 => (1, actions.get_rule_max().min(60)),
                1 => (1, 20),
                _ => (4, 80),
            };
            rows.push(RuleCondition { metric, op, value });
            update_rule_summary(&actions);
            return;
        }
        let index = index as usize;
        let Some(mut row) = rows.row_data(index) else { return };
        match field.as_str() {
            "remove" if rows.row_count() > 1 => {
                rows.remove(index);
                update_rule_summary(&actions);
                return;
            }
            "metric" if (0..3).contains(&value) && row.metric != value => {
                row.metric = value;
                row.value = match value { 0 => actions.get_rule_max().min(60), 1 => 20, _ => 80 };
                row.op = if value == 2 { 4 } else { 1 };
            }
            "operator" if (0..5).contains(&value) => row.op = value,
            "value" => row.value = value.clamp(0, if row.metric == 0 { actions.get_rule_max() } else { 100 }),
            _ => return,
        }
        if rows.row_data(index) != Some(row.clone()) {
            rows.set_row_data(index, row);
            update_rule_summary(&actions);
        }
    });
    let weak = view.as_weak();
    actions.on_save_rule(move || {
        let Some(view) = weak.upgrade() else { return };
        let actions = view.global::<Actions>();
        let draft = actions.get_rule_conditions();
        let conditions = draft.iter().map(rule_condition).collect::<Option<Vec<_>>>();
        let Some(conditions) = conditions.filter(|items| !items.is_empty()) else { return };
        let id = actions.get_rule_id();
        let logic = if actions.get_rule_logic() == 0 { Logic::And } else { Logic::Or };
        send(json!({"cmd":"alertRule", "op": if id == 0 { "add" } else { "edit" },
            "key":actions.get_rule_key().as_str(), "id":id, "logic":logic, "conditions":conditions}));
        actions.set_rule_key("".into());
    });
    let weak = view.as_weak();
    actions.on_delete_rule(move |key, id| {
        send(json!({"cmd":"alertRule","op":"delete","key":key.as_str(),"id":id}));
        if let Some(view) = weak.upgrade() { view.global::<Actions>().set_selected_rule_key("".into()); }
    });
}

pub fn run() -> Result<(), slint::PlatformError> {
    // 작업 표시줄: 본체(시작 메뉴 바로 가기)와 다른 앱 ID를 줘서, 바로 가기 아이콘 대신 창 아이콘(로고 색 설정)을 쓰게 함
    #[cfg(windows)]
    live::set_app_id();
    let view = SettingsView::new()?;
    #[cfg(all(target_os = "macos", feature = "external-fonts"))]
    load_bundled_fonts(view.window())?;
    #[cfg(windows)]
    {
        view.global::<Motion>().set_live_clock(true);
        let weak = view.as_weak();
        view.on_resizing(move || live::start(weak.clone()));
    }
    view.set_accounts(ModelRc::new(VecModel::default()));
    view.set_font_license(include_str!("../ui/fonts/OFL.txt").into());
    wire_actions(&view);
    view.show()?;
    let weak = view.as_weak();
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if weak
                .upgrade_in_event_loop(move |view| {
                    if message == "focus" {
                        view.window().with_winit_window(|w| {
                            w.set_minimized(false);
                            w.focus_window();
                        });
                    } else if let Some(preview) = message.get("widgetPreviewOnly") {
                        view_state::apply_widget_preview(&view, preview);
                    } else if message.is_object() {
                        // 언어: 영어면 번역 묶음, 한국어면 원문 ("")
                        let lang = if message["lang"] == "en" { usagi_core::i18n::Lang::En } else { usagi_core::i18n::Lang::Ko };
                        if lang != usagi_core::i18n::lang() || !LANG_SET.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            usagi_core::i18n::set(lang);
                            let _ = slint::select_bundled_translation(if lang == usagi_core::i18n::Lang::En { "en" } else { "" });
                        }
                        view_state::apply(&view, &message);
                    } else if cfg!(debug_assertions) {
                        // 개발용 화면 캡처: "page:N" 페이지 전환, "sidebar" 사이드바 펼치기·접기, "confirm" 확인 창
                        match message.as_str().unwrap_or_default() {
                            "sidebar" => view.set_sidebar_flipped(!view.get_sidebar_flipped()),
                            // "open:<이름>": 조건부 팝업 열기 (add:<키>, add-reset:<키>, edit:<키>:<종류>:<값>, basis:<키>, providers)
                            open if open.starts_with("open:") => {
                                view.global::<Debug>().set_open("".into());
                                view.global::<Debug>().set_open(open["open:".len()..].into());
                                // 그리는 도중에 열린 팝업은 다음 그리기 요청이 없으면 열림 애니메이션이 멈춰 있음 (캡처용으로 몇 번 다시 그림)
                                for ms in [40u64, 120, 260] {
                                    let weak = view.as_weak();
                                    slint::Timer::single_shot(std::time::Duration::from_millis(ms), move || {
                                        if let Some(v) = weak.upgrade() {
                                            v.window().request_redraw();
                                        }
                                    });
                                }
                            }
                            "confirm" => {
                                view.global::<Actions>().set_confirm_text(usagi_core::tr!("usAgI 종료 확인", "Quit usAgI?").into());
                                view.global::<Actions>().set_confirm_cmd("quit".into());
                            }
                            // "confirm:<cmd>": 설정 초기화·계정 삭제 확인 창
                            c if c.starts_with("confirm:") => {
                                let (cmd, id) = c["confirm:".len()..].split_once(':').unwrap_or((&c["confirm:".len()..], ""));
                                let text = match cmd {
                                    "resetSettings" => usagi_core::tr!("설정 초기화 확인", "Reset settings?"),
                                    _ => usagi_core::tr!("계정 삭제 확인", "Remove this account?"),
                                };
                                view.global::<Actions>().set_confirm_text(text.into());
                                view.global::<Actions>().set_confirm_cmd(cmd.into());
                                view.global::<Actions>().set_confirm_id(id.into());
                            }
                            other => {
                                if let Some(n) = other.strip_prefix("page:").and_then(|n| n.parse().ok()) {
                                    view.set_page(n);
                                } else if let Some(px) = other.strip_prefix("scroll:").and_then(|n| n.parse::<f32>().ok()) {
                                    view.set_debug_scroll(px);
                                }
                            }
                        }
                    }
                })
                .is_err()
            {
                return;
            }
        }
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    });
    send(json!({"cmd":"ready"}));
    slint::run_event_loop()
}

#[cfg(all(target_os = "macos", feature = "external-fonts"))]
fn load_bundled_fonts(window: &slint::Window) -> Result<(), slint::PlatformError> {
    // Slint 1.17 exposes dynamic font registration through its renderer.
    use slint::private_unstable_api::re_exports::WindowInner;

    let exe = std::env::current_exe().map_err(|e| slint::PlatformError::Other(e.to_string()))?;
    let contents = exe.parent().and_then(std::path::Path::parent)
        .ok_or_else(|| slint::PlatformError::Other("Missing app bundle".into()))?;
    let fonts = contents.join("Resources/Fonts");
    let adapter = WindowInner::from_pub(window).window_adapter();
    for name in ["UsagiUI-Medium.otf", "UsagiUI-SemiBold.otf"] {
        adapter.renderer().register_font_from_path(&fonts.join(name))
            .map_err(|e| slint::PlatformError::Other(e.to_string()))?;
    }
    Ok(())
}

pub fn main_window() {
    if let Err(e) = run() {
        send(json!({"cmd":"settingsError","error":e.to_string()}));
    }
}

/// Windows: 창 테두리를 끄는 동안(시스템 크기 조절 루프) Slint의 애니메이션·타이머 시계가 멈추므로,
/// 그동안만 스레드 타이머로 시계를 돌리고 다시 그림. 루프가 끝나면 타이머를 멈춤.
#[cfg(windows)]
mod live {
    use super::SettingsView;
    use std::cell::{Cell, RefCell};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetGUIThreadInfo, KillTimer, SetTimer, GUITHREADINFO, GUI_INMOVESIZE};

    thread_local! {
        static TIMER: Cell<usize> = const { Cell::new(0) };
        static VIEW: RefCell<Option<slint::Weak<SettingsView>>> = const { RefCell::new(None) };
    }

    pub fn set_app_id() {
        let id: Vec<u16> = "usAgI.Settings".encode_utf16().chain(Some(0)).collect();
        unsafe { SetCurrentProcessExplicitAppUserModelID(id.as_ptr()) };
    }

    fn sizing() -> bool {
        let mut info: GUITHREADINFO = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        unsafe { GetGUIThreadInfo(GetCurrentThreadId(), &mut info) != 0 && info.flags & GUI_INMOVESIZE != 0 }
    }

    pub fn start(view: slint::Weak<SettingsView>) {
        VIEW.with(|v| *v.borrow_mut() = Some(view));
        if TIMER.get() == 0 && sizing() {
            // 약 60fps
            TIMER.set(unsafe { SetTimer(std::ptr::null_mut(), 0, 15, Some(tick)) });
        }
    }

    unsafe extern "system" fn tick(_: HWND, _: u32, id: usize, _: u32) {
        if !sizing() {
            unsafe { KillTimer(std::ptr::null_mut(), id) };
            TIMER.set(0);
            return;
        }
        // 직전 그리기에서 애니메이션이 돌고 있었을 때만 다시 그림 (그 외에는 크기 변화가 알아서 다시 그림)
        VIEW.with(|v| {
            let view = v.borrow().as_ref().and_then(|w| w.upgrade());
            use slint::ComponentHandle;
            let animating = view.as_ref().is_some_and(|view| view.window().has_active_animations());
            slint::platform::update_timers_and_animations();
            if let (true, Some(view)) = (animating, view) {
                view.window().request_redraw();
            }
        });
    }
}
