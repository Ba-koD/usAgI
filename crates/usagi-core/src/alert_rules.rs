//! Graph rules with explicit comparisons. Legacy threshold lists share only the graph's on/off switch.
use crate::{config::State, engine::{Meter, reset_lead_max}, texts, tr};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Metric { Time, Left, Used }

impl Metric {
    pub fn label(self) -> &'static str {
        match self {
            Self::Time => tr!("리셋까지 남은 시간", "Time until reset"),
            Self::Left => tr!("남은 사용량", "Remaining usage"),
            Self::Used => tr!("사용량", "Usage"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Comparison { Lt, Le, Eq, Gt, Ge }

impl Comparison {
    pub fn symbol(self) -> &'static str {
        match self { Self::Lt => "<", Self::Le => "≤", Self::Eq => "=", Self::Gt => ">", Self::Ge => "≥" }
    }

    fn matches(self, actual: u32, target: u32) -> bool {
        match self { Self::Lt => actual < target, Self::Le => actual <= target, Self::Eq => actual == target,
            Self::Gt => actual > target, Self::Ge => actual >= target }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Logic { And, Or }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Condition {
    pub metric: Metric,
    pub op: Comparison,
    pub value: u32,
}

impl Condition {
    pub fn amount(self) -> String {
        if self.metric == Metric::Time { texts::fmt_lead(f64::from(self.value)) }
        else { format!("{}%", self.value) }
    }

    pub fn description(self) -> String {
        let amount = self.amount();
        let operator = match self.op {
            Comparison::Lt => tr!("미만", "less than"),
            Comparison::Le => tr!("이하", "at most"),
            Comparison::Eq => tr!("같음", "equal to"),
            Comparison::Gt => tr!("초과", "greater than"),
            Comparison::Ge => tr!("이상", "at least"),
        };
        tr!(format!("{amount} {operator}"), format!("{operator} {amount}"))
    }

    fn matches(self, meter: &Meter, now: f64) -> bool {
        if self.metric == Metric::Time {
            let Some(seconds) = meter.resets_at.map(|reset| reset - now).filter(|&left| left.is_finite() && left > 0.0) else { return false };
            let limit = f64::from(self.value) * 60.0;
            return match self.op {
                Comparison::Lt => seconds < limit,
                Comparison::Le => seconds <= limit,
                // Equality covers the displayed minute without firing after a reset.
                Comparison::Eq if self.value == 0 => seconds < 60.0,
                Comparison::Eq => seconds <= limit && seconds > limit - 60.0,
                Comparison::Gt => seconds > limit,
                Comparison::Ge => seconds >= limit,
            };
        }
        let used = (meter.ratio.clamp(0.0, 1.0) * 100.0).round() as u32;
        meter.ratio.is_finite() && self.op.matches(if self.metric == Metric::Used { used } else { 100 - used }, self.value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: u32,
    pub logic: Logic,
    pub conditions: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_reset: Option<i64>,
}

impl Rule {
    fn valid(&self, key: &str) -> bool {
        (1..=i32::MAX as u32).contains(&self.id) && !self.conditions.is_empty() && self.conditions.iter().all(|c| c.value <=
            if c.metric == Metric::Time { reset_lead_max(key) } else { 100 })
    }

    fn matches(&self, meter: &Meter, now: f64) -> bool {
        let mut conditions = self.conditions.iter().map(|c| c.matches(meter, now));
        match self.logic { Logic::And => conditions.all(|m| m), Logic::Or => conditions.any(|m| m) }
    }
}

pub fn rules(state: &State, key: &str) -> Vec<Rule> {
    state.0.get("alerts").and_then(|v| v.get(key)).and_then(|v| v["rules"].as_array())
        .into_iter().flatten()
        .filter_map(|value| Rule::deserialize(value).ok())
        .filter(|rule| rule.valid(key)).collect()
}

pub(crate) fn apply(state: &mut State, key: &str, msg: &Value) {
    let mut items = rules(state, key);
    let id = msg["id"].as_u64().and_then(|n| u32::try_from(n).ok());
    let new = msg["op"] == "add";
    if msg["op"] == "delete" {
        let Some(index) = items.iter().position(|rule| Some(rule.id) == id) else { return };
        items.remove(index);
    } else {
        let Some((id, last_reset)) = (if new {
            items.iter().map(|r| r.id).max().unwrap_or(0).checked_add(1).map(|id| (id, None))
        } else if msg["op"] == "edit" {
            items.iter().find(|r| Some(r.id) == id).map(|r| (r.id, r.last_reset))
        } else { None }) else { return };
        let Ok(logic) = Logic::deserialize(&msg["logic"]) else { return };
        let Ok(conditions) = Vec::<Condition>::deserialize(&msg["conditions"]) else { return };
        let rule = Rule { id, logic, conditions, last_reset };
        if !rule.valid(key) { return; }
        if new { items.push(rule); }
        else if let Some(old) = items.iter_mut().find(|r| r.id == id) { *old = rule; }
    }
    let alerts = state.0.entry("alerts").or_insert_with(|| json!({}));
    if !alerts.is_object() { *alerts = json!({}); }
    let value = alerts.as_object_mut().unwrap().entry(key).or_insert_with(|| json!({}));
    if !value.is_object() { *value = json!({}); }
    if items.is_empty() { value.as_object_mut().unwrap().remove("rules"); }
    else { value["rules"] = json!(items); }
}

pub(crate) fn due(state: &mut State, key: &str, meter: &Meter, now: f64) -> Vec<Rule> {
    if !meter.exact || meter.resets_at.is_some_and(|reset| reset <= now) { return Vec::new(); }
    let Some(graph) = state.0.get_mut("alerts").and_then(|v| v.get_mut(key)) else { return Vec::new() };
    if graph.get("on").and_then(Value::as_bool) == Some(false) { return Vec::new(); }
    // Mutable indexing panics when the stored entry is not an object.
    let Some(items) = graph.get_mut("rules").and_then(Value::as_array_mut) else { return Vec::new() };
    // Use the provider's reset time when available, otherwise keep the legacy daily window.
    let stamp = stamp(meter, now);
    let mut triggered = Vec::new();
    for item in items {
        let Ok(rule) = Rule::deserialize(&*item) else { continue };
        if !seen(&rule, stamp) && rule.valid(key) && rule.matches(meter, now) {
            item["last_reset"] = json!(stamp);
            triggered.push(rule);
        }
    }
    triggered
}

fn stamp(meter: &Meter, now: f64) -> i64 {
    meter.resets_at.map_or_else(|| -((now / 86400.0).floor() as i64) - 1, |t| t.round() as i64)
}

fn seen(rule: &Rule, stamp: i64) -> bool {
    rule.last_reset.is_some_and(|last| last == stamp || (last > 0 && stamp > 0 && last.abs_diff(stamp) <= 10))
}

/// Time comparisons can become true without a new usage reading.
pub(crate) fn next_check(state: &State, snapshot: &crate::engine::Snapshot, now: f64) -> Option<f64> {
    let alerts = state.0.get("alerts")?;
    let mut earliest = None;
    for provider in &snapshot.providers {
        for meter in provider.meters.iter().filter(|m| m.exact) {
            let Some(reset) = meter.resets_at.filter(|&reset| reset.is_finite() && reset > now) else { continue };
            let key = crate::engine::alert_key(&provider.account_id, &meter.label);
            if alerts[&key]["on"].as_bool() == Some(false) { continue; }
            for rule in rules(state, &key).iter().filter(|r| !seen(r, stamp(meter, now))) {
                for condition in rule.conditions.iter().filter(|c| c.metric == Metric::Time) {
                    let minutes = f64::from(condition.value);
                    let at = match condition.op {
                        Comparison::Le => reset - minutes * 60.0,
                        Comparison::Lt => reset - minutes * 60.0 + 0.02,
                        Comparison::Eq if condition.value == 0 => reset - 60.0 + 0.02,
                        Comparison::Eq => reset - minutes * 60.0,
                        _ => continue,
                    };
                    if at > now && at < reset {
                        earliest = Some(earliest.map_or(at, |previous: f64| previous.min(at)));
                    }
                }
            }
        }
    }
    earliest
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &str = "claude-1:5시간";

    fn state() -> State {
        let mut state = State::default();
        state.0.insert("alerts".into(), json!({KEY: {"on":true, "at":80, "at_off":[85], "before":[20], "before_off":[30]}}));
        state
    }

    fn update(state: &mut State, op: &str, id: u32, logic: &str, limit: u32) {
        apply(state, KEY, &json!({"op":op,"id":id,"logic":logic,"conditions":[
            {"metric":"time","op":"le","value":80}, {"metric":"left","op":"le","value":limit}
        ]}));
    }

    #[test]
    fn edits_and_deletes_only_the_selected_compound_rule() {
        let mut state = state();
        let legacy = state.0["alerts"][KEY].clone();
        update(&mut state, "add", 0, "and", 20);
        update(&mut state, "add", 0, "or", 30);
        update(&mut state, "edit", 1, "or", 50);
        update(&mut state, "edit", 1, "and", 101);
        assert_eq!(rules(&state, KEY)[0].conditions[1].value, 50);
        assert_eq!(rules(&state, KEY)[0].logic, Logic::Or);
        update(&mut state, "delete", 1, "", 0);
        assert_eq!(rules(&state, KEY)[0].id, 2);
        update(&mut state, "delete", 2, "", 0);
        assert_eq!(state.0["alerts"][KEY], legacy);
    }

    #[test]
    fn compound_conditions_fire_once_per_reset_and_respect_disabled_graphs() {
        let mut state = state();
        update(&mut state, "add", 0, "and", 20);
        update(&mut state, "add", 0, "or", 20);
        let mut meter = Meter { label: "5시간".into(), ratio: 0.82, resets_at: Some(50_000.0), exact: true };
        let before = 50_000.0 - 80.0 * 60.0 - 0.1;
        assert_eq!(due(&mut state, KEY, &meter, before)[0].id, 2);
        let at = 50_000.0 - 80.0 * 60.0;
        assert_eq!(due(&mut state, KEY, &meter, at)[0].id, 1);
        update(&mut state, "edit", 1, "or", 25);
        assert!(due(&mut state, KEY, &meter, at + 20.0).is_empty());
        meter.resets_at = Some(70_000.0);
        state.0.get_mut("alerts").unwrap()[KEY]["on"] = json!(false);
        assert!(due(&mut state, KEY, &meter, 68_000.0).is_empty());
        state.0.get_mut("alerts").unwrap()[KEY]["on"] = json!(true);
        assert_eq!(due(&mut state, KEY, &meter, 68_000.0).len(), 2);
    }

    #[test]
    fn a_graph_entry_that_is_not_an_object_has_no_due_rules() {
        let meter = Meter { label: "5시간".into(), ratio: 0.82, resets_at: Some(50_000.0), exact: true };
        for entry in [json!(5), json!("on"), json!([1]), json!(null), json!({"rules": 7})] {
            let mut state = State::default();
            state.0.insert("alerts".into(), json!({KEY: entry}));
            assert!(due(&mut state, KEY, &meter, 40_000.0).is_empty());
            assert!(rules(&state, KEY).is_empty());
        }
    }

    #[test]
    fn time_operators_distinguish_exact_boundary_and_equality_minute() {
        let meter = Meter { label: "5시간".into(), ratio: 0.5, resets_at: Some(5000.0), exact: true };
        let condition = |op| Condition { metric: Metric::Time, op, value: 80 };
        assert!(!condition(Comparison::Le).matches(&meter, 199.9));
        assert!(condition(Comparison::Le).matches(&meter, 200.0));
        assert!(!condition(Comparison::Lt).matches(&meter, 200.0));
        assert!(condition(Comparison::Lt).matches(&meter, 200.1));
        assert!(condition(Comparison::Eq).matches(&meter, 259.9));
        assert!(!condition(Comparison::Eq).matches(&meter, 260.0));
    }
}
