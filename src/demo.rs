//! Deterministic, credential-free view states for screenshots and UI tests.

use std::sync::OnceLock;

use crate::gfx::LimitRow;
use crate::gfx::{CapsControl, FlyoutData, Section, SectionBody, SettingsView, View};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    ClaudeOnly,
    CodexOnly,
    Both,
    Loading,
    Stale,
    Cooldown,
    Error,
    Neither,
    Settings,
    Many,
}

impl Scenario {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "claude-only" => Some(Self::ClaudeOnly),
            "codex-only" => Some(Self::CodexOnly),
            "both" | "dual-provider" => Some(Self::Both),
            "loading" => Some(Self::Loading),
            "stale" => Some(Self::Stale),
            "cooldown" => Some(Self::Cooldown),
            "error" => Some(Self::Error),
            "neither" => Some(Self::Neither),
            "settings" => Some(Self::Settings),
            "many" => Some(Self::Many),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub scenario: Scenario,
    pub hidden: bool,
    pub light: bool,
    pub contrast: bool,
    pub ready_event: Option<String>,
}

pub struct State {
    pub scenario: Scenario,
    pub view: View,
    pub fetching: bool,
    pub hidden: bool,
    pub light: bool,
    pub contrast: bool,
    pub tray_percent: Option<f32>,
    pub tray_tip: &'static str,
    pub ready_event: Option<String>,
}

static ACTIVE: OnceLock<State> = OnceLock::new();

pub fn parse_request(args: &[String]) -> Result<Option<Request>, String> {
    let mut scenario = None;
    let mut hidden = false;
    let mut light = false;
    let mut contrast = false;
    let mut ready_event = None;
    let mut index = 1;
    while index < args.len() {
        let argument = &args[index];
        if argument == "--demo-hidden" {
            hidden = true;
        } else if argument == "--demo-light" {
            light = true;
        } else if argument == "--demo-contrast" {
            contrast = true;
        } else if let Some(value) = argument.strip_prefix("--demo-ready-event=") {
            if !valid_ready_event(value) {
                return Err("invalid demo readiness event name".to_string());
            }
            ready_event = Some(value.to_string());
        } else if let Some(value) = argument.strip_prefix("--demo=") {
            scenario = Some(parse_scenario(value)?);
        } else if argument == "--demo" {
            index += 1;
            let value = args
                .get(index)
                .ok_or_else(|| "--demo requires a scenario".to_string())?;
            scenario = Some(parse_scenario(value)?);
        }
        index += 1;
    }

    if hidden && scenario.is_none() {
        return Err("--demo-hidden requires --demo".to_string());
    }
    if light && scenario.is_none() {
        return Err("--demo-light requires --demo".to_string());
    }
    if contrast && scenario.is_none() {
        return Err("--demo-contrast requires --demo".to_string());
    }
    if contrast && light {
        return Err("--demo-contrast and --demo-light cannot be combined".to_string());
    }
    if ready_event.is_some() && scenario.is_none() {
        return Err("--demo-ready-event requires --demo".to_string());
    }
    Ok(scenario.map(|scenario| Request {
        scenario,
        hidden,
        light,
        contrast,
        ready_event,
    }))
}

fn valid_ready_event(value: &str) -> bool {
    let Some(nonce) = value.strip_prefix("Local\\Claudometer.DemoReady.") else {
        return false;
    };
    !nonce.is_empty()
        && nonce.len() <= 64
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn parse_scenario(value: &str) -> Result<Scenario, String> {
    Scenario::parse(value).ok_or_else(|| {
        format!(
            "unknown demo scenario '{value}'; expected claude-only, codex-only, both, loading, stale, cooldown, error, neither, settings, or many"
        )
    })
}

pub fn activate(request: Request, now_unix: i64) -> Result<(), &'static str> {
    ACTIVE
        .set(build_state(request, now_unix))
        .map_err(|_| "demo mode already active")
}

pub fn active() -> Option<&'static State> {
    ACTIVE.get()
}

pub fn is_active() -> bool {
    active().is_some()
}

fn build_state(request: Request, now_unix: i64) -> State {
    let claude = || provider_section("Claude", "Max", 36.0, 58.0, now_unix);
    let codex = || provider_section("Codex", "Plus", 64.0, 41.0, now_unix);

    let (view, fetching, tray_percent, tray_tip) = match request.scenario {
        Scenario::ClaudeOnly => (
            data(vec![claude()], now_unix, None),
            false,
            Some(0.36),
            "Demo · Claude session 36%",
        ),
        Scenario::CodexOnly => (
            data(vec![codex()], now_unix, None),
            false,
            Some(0.64),
            "Demo · Codex session 64%",
        ),
        Scenario::Both => (
            data(vec![claude(), codex()], now_unix, None),
            false,
            Some(0.64),
            "Demo · highest session 64%",
        ),
        Scenario::Loading => (View::Loading, true, None, "Demo · loading usage"),
        Scenario::Stale => (
            data(
                vec![claude(), codex()],
                now_unix - 7 * 60,
                Some("Compatibility source · cached 7m ago"),
            ),
            false,
            Some(0.64),
            "Demo · cached usage",
        ),
        Scenario::Cooldown => (
            data(
                vec![claude(), codex()],
                now_unix - 2 * 60,
                Some("Rate limited · retry available in 2m"),
            ),
            false,
            Some(0.64),
            "Demo · retry in 2m",
        ),
        Scenario::Error => (
            View::Error(
                "Offline\nCan't reach api.anthropic.com. Showing the last values.".to_string(),
            ),
            false,
            None,
            "Demo · usage unavailable",
        ),
        Scenario::Neither => (
            View::Data(FlyoutData {
                sections: vec![Section {
                    title: "Providers",
                    plan: String::new(),
                    status: None,
                    body: SectionBody::Note("Claude and Codex are not signed in".to_string()),
                }],
                fetched_unix: None,
                note: Some("Open Settings to get started".to_string()),
            }),
            false,
            None,
            "Demo · no providers ready",
        ),
        Scenario::Settings => (View::Loading, false, None, "Demo · settings"),
        Scenario::Many => {
            let mut section = claude();
            if let SectionBody::Rows(rows) = &mut section.body {
                for index in 0..16 {
                    rows.push(LimitRow {
                        label: format!("Weekly · model {}", index + 1),
                        percent: 20.0 + (index * 4) as f64,
                        severity: None,
                        reset_text: "resets in 4d".to_string(),
                    });
                }
            }
            (
                data(vec![section], now_unix, None),
                false,
                Some(0.36),
                "Demo · many limits",
            )
        }
    };

    State {
        scenario: request.scenario,
        view,
        fetching,
        hidden: request.hidden,
        light: request.light,
        contrast: request.contrast,
        tray_percent,
        tray_tip,
        ready_event: request.ready_event,
    }
}

pub fn settings_view(hover: i32, focus: i32) -> SettingsView {
    SettingsView {
        account_caption: "Connected account · Max".to_string(),
        account_action: "Reconnect",
        account_connected: true,
        caps_caption: "Installed · enabled".to_string(),
        caps_control: CapsControl::Toggle(true),
        autostart: true,
        codex_on: true,
        alerts_on: true,
        update_checks_on: false,
        lid_label: "Advanced · ignore lid close".to_string(),
        lid_caption: "Restores on exit when enabled".to_string(),
        lid_on: false,
        lid_action: None,
        about: format!("Claudometer {}", env!("CARGO_PKG_VERSION")),
        about_btn: "GitHub",
        update_ready: false,
        poll_secs: 60,
        refresh_label: "Refresh usage now".to_string(),
        hover,
        focus,
    }
}

fn data(sections: Vec<Section>, fetched_unix: i64, note: Option<&str>) -> View {
    View::Data(FlyoutData {
        sections,
        fetched_unix: Some(fetched_unix),
        note: note.map(str::to_string),
    })
}

fn provider_section(
    title: &'static str,
    plan: &str,
    session: f64,
    weekly: f64,
    _now_unix: i64,
) -> Section {
    Section {
        title,
        plan: plan.to_string(),
        status: None,
        body: SectionBody::Rows(vec![
            LimitRow {
                label: "Session (5h)".to_string(),
                percent: session,
                severity: None,
                reset_text: "resets in 2h 15m".to_string(),
            },
            LimitRow {
                label: "Weekly · all models".to_string(),
                percent: weekly,
                severity: None,
                reset_text: "resets in 4d".to_string(),
            },
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_documented_scenarios_and_hidden_mode() {
        for name in [
            "claude-only",
            "codex-only",
            "both",
            "dual-provider",
            "loading",
            "stale",
            "cooldown",
            "error",
            "neither",
            "settings",
            "many",
        ] {
            let args = vec!["claudometer".to_string(), format!("--demo={name}")];
            assert!(parse_request(&args).unwrap().is_some());
        }
        let args = vec![
            "claudometer".to_string(),
            "--demo".to_string(),
            "both".to_string(),
            "--demo-hidden".to_string(),
            "--demo-light".to_string(),
            "--demo-ready-event=Local\\Claudometer.DemoReady.test-123".to_string(),
        ];
        let request = parse_request(&args).unwrap().unwrap();
        assert!(request.hidden);
        assert!(request.light);
        assert_eq!(
            request.ready_event.as_deref(),
            Some("Local\\Claudometer.DemoReady.test-123")
        );
    }

    #[test]
    fn rejects_incomplete_or_unknown_demo_requests() {
        assert!(parse_request(&["claudometer".to_string(), "--demo".to_string()]).is_err());
        assert!(
            parse_request(&["claudometer".to_string(), "--demo=surprise".to_string()]).is_err()
        );
        assert!(parse_request(&["claudometer".to_string(), "--demo-hidden".to_string()]).is_err());
        assert!(parse_request(&[
            "claudometer".to_string(),
            "--demo=both".to_string(),
            "--demo-ready-event=Global\\untrusted".to_string(),
        ])
        .is_err());
    }

    #[test]
    fn every_state_is_built_from_synthetic_bounded_data() {
        for scenario in [
            Scenario::ClaudeOnly,
            Scenario::CodexOnly,
            Scenario::Both,
            Scenario::Loading,
            Scenario::Stale,
            Scenario::Cooldown,
            Scenario::Error,
            Scenario::Neither,
            Scenario::Settings,
            Scenario::Many,
        ] {
            let state = build_state(
                Request {
                    scenario,
                    hidden: false,
                    light: false,
                    contrast: false,
                    ready_event: None,
                },
                1_788_400_000,
            );
            if let View::Data(data) = state.view {
                assert!(data.sections.len() <= 2);
                for section in data.sections {
                    if let SectionBody::Rows(rows) = section.body {
                        assert!(rows.len() <= if scenario == Scenario::Many { 18 } else { 2 });
                    }
                }
            }
        }
    }
}
