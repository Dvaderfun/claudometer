//! Rendering: D3D11 + DXGI composition swapchain (premultiplied alpha) plus a
//! DirectComposition visual carrying Direct2D/DirectWrite content.
//!
//! `Surface` is one alpha-composited window canvas — used by both the flyout
//! and the settings window. DWM/accent-policy draws the material behind it.
//!
//! Resources are cached: one RT cast, text formats built once, solid brushes
//! rebuilt only when theme, accent, or contrast colors change. Surfaces themselves are dropped
//! by main.rs when their window hides — the GPU stack is the RAM cost.
//!
//! Visuals follow the Fluent type ramp and 4px spacing grid.

use windows::core::*;
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use crate::provider::model::{ProviderSeverity, UsageLimit};
use crate::provider::pace::{pace, Pace};

#[derive(Clone)]
pub struct LimitRow {
    pub label: String,
    pub percent: f64,
    pub severity: Option<ProviderSeverity>,
    pub reset_text: String,
    pub pace: Pace,
}

impl From<UsageLimit> for LimitRow {
    fn from(row: UsageLimit) -> Self {
        Self {
            label: row.label,
            percent: row.percent.get(),
            severity: row.severity,
            reset_text: row
                .resets_unix
                .map(crate::api::fmt_reset_unix)
                .unwrap_or_default(),
            pace: Pace::Level,
        }
    }
}

impl LimitRow {
    pub fn with_pace(
        row: UsageLimit,
        provider: crate::provider::model::ProviderId,
        now_unix: i64,
        enabled: bool,
    ) -> Self {
        let not_started = provider == crate::provider::model::ProviderId::Claude
            && row.kind == crate::provider::model::LimitKind::Session
            && row.resets_unix.is_none();
        let verdict = if enabled {
            pace(&row, now_unix)
        } else {
            Pace::Level
        };
        let mut result = Self::from(row);
        if not_started {
            result.reset_text = "Not started".into();
        }
        result.apply_pace(verdict, now_unix);
        result
    }

    pub fn apply_pace(&mut self, verdict: Pace, now_unix: i64) {
        self.pace = verdict;
        let note: std::borrow::Cow<'static, str> = match verdict {
            Pace::LimitReached => "Limit reached".into(),
            Pace::Over {
                limit_at_unix: Some(at),
                ..
            } => crate::api::fmt_limit_unix(at, now_unix).into(),
            Pace::Over {
                limit_at_unix: None,
                ..
            } => "At limit by reset".into(),
            Pace::Tight { spare_percent, .. } => format!("~{spare_percent}% spare").into(),
            _ => "".into(),
        };
        if !note.is_empty() {
            self.label.push_str(" · ");
            self.label.push_str(&note);
        }
    }
}
use crate::provider::state::Clock;
use crate::util;

/// One provider block in the flyout: header (name + plan) and either limit
/// rows or a single dim status line (loading / per-provider error).
#[derive(Clone)]
pub struct Section {
    pub title: &'static str,
    pub plan: String,
    pub status: Option<String>,
    pub help: String,
    pub body: SectionBody,
}

#[derive(Clone)]
pub enum SectionBody {
    Rows(Vec<LimitRow>),
    Note(String),
}

#[derive(Clone)]
pub struct FlyoutData {
    pub sections: Vec<Section>,
    /// most recent successful fetch across sections; None = nothing fetched yet
    pub fetched_unix: Option<i64>,
    /// footer note (stale-data errors), e.g. "Codex: rate limited"
    pub note: Option<String>,
    pub next_update_unix: Option<i64>,
    pub retry_at_unix: Option<i64>,
}

#[derive(Clone)]
pub enum View {
    Loading,
    Error(String),
    Data(FlyoutData),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FlyHover {
    None,
    Footer,
    Refresh,
    Gear,
    Vibe,
}

#[derive(Clone, Copy)]
pub enum CapsControl {
    Unavailable,
    Toggle(bool),
    Retry,
}

pub struct SettingsView {
    pub diagnostics: String,
    pub diagnostics_copy: &'static str,
    pub account_caption: String,
    pub account_action: &'static str,
    pub account_connected: bool,
    pub caps_caption: String,
    pub caps_control: CapsControl,
    pub autostart: bool,
    pub codex_on: bool,
    pub codex_server_on: bool,
    pub pace_on: bool,
    pub reset_format: crate::config::ResetFormat,
    pub quota_display: crate::config::QuotaDisplay,
    pub alerts_on: bool,
    pub update_checks_on: bool,
    pub lid_label: String,
    pub lid_caption: String,
    pub lid_on: bool,
    pub lid_action: Option<&'static str>,
    /// About-card label ("Claudometer 0.4.0" / "Update v0.5.0 available" / …)
    pub about: String,
    /// About-card button text ("GitHub" / "Install" / "…")
    pub about_btn: &'static str,
    /// accent-tint the About icon when an update is ready
    pub update_ready: bool,
    pub poll_secs: u32,
    pub refresh_label: String,
    pub hover: i32, // card index, -1 = none
    pub focus: i32, // keyboard focus card index, -1 = none
}

impl SettingsView {
    pub fn row_choice(&self, card: usize) -> &'static str {
        if card == CARD_RESET_FORMAT {
            self.reset_format.label()
        } else {
            self.quota_display.label()
        }
    }
}

pub fn quota_value(percent: f64) -> String {
    format_quota_value(percent, crate::config::settings().quota_display)
}

fn format_quota_value(percent: f64, display: crate::config::QuotaDisplay) -> String {
    let (value, word) = if display == crate::config::QuotaDisplay::Left {
        (100.0 - percent, "left")
    } else {
        (percent, "used")
    };
    let mut text = format!("{value:.0}% ");
    text.push_str(word);
    text
}

impl SettingsView {
    fn toggle_for(&self, card: usize) -> Option<bool> {
        match card {
            CARD_AUTOSTART => Some(self.autostart),
            CARD_CODEX => Some(self.codex_on),
            CARD_ALERTS => Some(self.alerts_on),
            CARD_UPDATE_CHECKS => Some(self.update_checks_on),
            CARD_CODEX_SERVER => Some(self.codex_server_on),
            CARD_PACE => Some(self.pace_on),
            _ => None,
        }
    }
}

pub const INTERVALS: [(u32, &str); 4] = [(30, "30s"), (60, "1m"), (120, "2m"), (300, "5m")];

// ---------- flyout layout (DIP, 4px grid) ----------

pub const FLYOUT_W: f32 = 328.0;
pub const FLYOUT_MAX_H: f32 = 720.0;
const PAD: f32 = 16.0;
const TITLE_H: f32 = 20.0;
const SECTION_GAP: f32 = 16.0;
const LABEL_H: f32 = 20.0;
const BAR_H: f32 = 4.0;
const GAP: f32 = 8.0;
const CAPTION_H: f32 = 16.0;
const ROW_GAP: f32 = 16.0;
const FOOTER_GAP_ABOVE: f32 = 12.0;
const FOOTER_GAP_BELOW: f32 = 8.0;
const ROW_BLOCK: f32 = LABEL_H + GAP + BAR_H + GAP + CAPTION_H;
/// breathing room above and below the divider between provider sections
const SEC_GAP: f32 = 16.0;

const SIZE_BODY: f32 = 14.0;
const SIZE_CAPTION: f32 = 12.0;

const BTN: f32 = 28.0; // header icon button

/// Vibecode row: label + caption + toggle, on the settings-card grid.
const VIBE_H: f32 = 60.0;
const VIBE_GAP: f32 = 12.0;
/// Bottom of the loading/error message block (head + wrapped body).
const MSG_H: f32 = 108.0;

/// Height of the view's own content — everything above the Vibecode row.
fn content_h(view: &View) -> f32 {
    match view {
        View::Data(d) => {
            let mut h = PAD;
            for (i, sec) in d.sections.iter().enumerate() {
                if i > 0 {
                    h += SEC_GAP + 1.0 + SEC_GAP;
                }
                h += TITLE_H + SECTION_GAP + section_body_h(&sec.body);
                if sec.status.is_some() {
                    h += CAPTION_H + GAP;
                }
            }
            h
        }
        _ => MSG_H,
    }
}

/// Vibecode toggle row, spanning the flyout's content width. Present in every
/// view — the mode is useful exactly when usage can't be fetched, too.
pub fn vibe_row(view: &View) -> D2D_RECT_F {
    vibe_row_at(content_h(view) + VIBE_GAP)
}

/// Same row from a cached top edge (hit-testing, without rebuilding the view).
pub fn vibe_row_at(top: f32) -> D2D_RECT_F {
    rect(PAD, top, FLYOUT_W - PAD, top + VIBE_H)
}

pub fn flyout_height(view: &View) -> f32 {
    footer_action_rect(view).bottom + PAD
}

pub fn footer_action_rect(view: &View) -> D2D_RECT_F {
    let top = vibe_row(view).bottom + FOOTER_GAP_ABOVE + 1.0 + FOOTER_GAP_BELOW;
    rect(
        PAD,
        top + CAPTION_H + GAP,
        FLYOUT_W - PAD,
        top + 2.0 * CAPTION_H + 2.0 * GAP,
    )
}

pub fn age_text(age: Option<i64>) -> String {
    match age {
        None => "never".into(),
        Some(age) if age < 60 => "just now".into(),
        Some(age) if age < 3600 => format!("{}m ago", age / 60),
        Some(age) => format!("{}h ago", age / 3600),
    }
}

pub fn footer_action(view: &View, now: i64) -> String {
    if let View::Data(data) = view {
        if let Some(at) = data.retry_at_unix.filter(|at| *at > now) {
            return format!("Retry at {}", crate::api::fmt_unix_hhmm(at));
        }
        if let Some(at) = data.next_update_unix.filter(|at| *at > now) {
            let minutes = at.saturating_sub(now).saturating_add(59) / 60;
            return format!("Next update in {minutes}m");
        }
    }
    "Refresh usage now".into()
}

pub fn updated_caption(view: &View) -> String {
    if let View::Data(data) = view {
        if let Some(at) = data.fetched_unix {
            let prefix = if data
                .sections
                .iter()
                .any(|section| section.help.contains("Cached values"))
            {
                "Cached values · "
            } else {
                ""
            };
            return format!("{prefix}Updated {}", relative_time(at));
        }
        if let Some(note) = &data.note {
            return note.clone();
        }
    }
    "No successful update yet".into()
}

fn section_body_h(body: &SectionBody) -> f32 {
    match body {
        SectionBody::Rows(rows) => {
            let n = rows.len().max(1) as f32;
            n * ROW_BLOCK + (n - 1.0) * ROW_GAP
        }
        SectionBody::Note(_) => CAPTION_H,
    }
}

/// Text bounds and spoken labels share the flyout's layout math, so assistive
/// clients read each displayed limit and its reset time in the right order.
pub fn accessible_rows(view: &View) -> Vec<(D2D_RECT_F, String)> {
    let mut rows = Vec::new();
    let View::Data(data) = view else {
        let label = match view {
            View::Loading => "Loading usage".to_string(),
            View::Error(message) => message.replace('\n', ". "),
            View::Data(_) => unreachable!(),
        };
        rows.push((rect(PAD, PAD, FLYOUT_W - PAD, MSG_H), label));
        return rows;
    };
    let mut y = PAD;
    for (section_index, section) in data.sections.iter().enumerate() {
        if section_index > 0 {
            y += SEC_GAP + 1.0 + SEC_GAP;
        }
        y += TITLE_H + SECTION_GAP;
        if let Some(status) = &section.status {
            rows.push((
                rect(PAD, y, FLYOUT_W - PAD, y + CAPTION_H),
                format!("{}, {status}", section.title),
            ));
            y += CAPTION_H + GAP;
        }
        match &section.body {
            SectionBody::Rows(limits) => {
                for (index, limit) in limits.iter().enumerate() {
                    if index > 0 {
                        y += ROW_GAP;
                    }
                    let mut name = format!("{}, {}", section.title, limit.label);
                    name.push_str(", ");
                    name.push_str(&quota_value(limit.percent));
                    if !limit.reset_text.is_empty() {
                        name.push_str(", ");
                        name.push_str(&limit.reset_text);
                        if limit.reset_text == "Not started" {
                            name.push_str(". The session starts with your first message.");
                        }
                    }
                    if !matches!(limit.pace, Pace::Level) {
                        name.push_str(", ");
                        name.push_str(limit.pace.name());
                    }
                    rows.push((rect(PAD, y, FLYOUT_W - PAD, y + ROW_BLOCK), name));
                    y += ROW_BLOCK;
                }
            }
            SectionBody::Note(message) => {
                rows.push((
                    rect(PAD, y, FLYOUT_W - PAD, y + CAPTION_H),
                    format!("{}, {message}", section.title),
                ));
                y += CAPTION_H;
            }
        }
    }
    let r = footer_action_rect(view);
    rows.push((
        rect(PAD, r.top - CAPTION_H - GAP, FLYOUT_W - PAD, r.top - GAP),
        updated_caption(view),
    ));
    rows
}

/// Mouse-only shortcuts use the same row bounds as UIA, with scroll applied by the caller.
pub fn row_shortcut(view: &View, x: f32, y: f32) -> Option<usize> {
    let View::Data(data) = view else {
        return None;
    };
    if !(PAD..FLYOUT_W - PAD).contains(&x) {
        return None;
    }
    let mut top = PAD;
    for (index, section) in data.sections.iter().enumerate() {
        if index > 0 {
            top += SEC_GAP + 1.0 + SEC_GAP;
        }
        top += TITLE_H + SECTION_GAP;
        if section.status.is_some() {
            top += CAPTION_H + GAP;
        }
        match &section.body {
            SectionBody::Rows(rows) => {
                for (index, row) in rows.iter().enumerate() {
                    if index > 0 {
                        top += ROW_GAP;
                    }
                    if (top + ROW_BLOCK - CAPTION_H..top + ROW_BLOCK).contains(&y) {
                        return if x < PAD + 96.0 {
                            Some(CARD_QUOTA_DISPLAY)
                        } else {
                            (!row.reset_text.is_empty()).then_some(CARD_RESET_FORMAT)
                        };
                    }
                    top += ROW_BLOCK;
                }
            }
            SectionBody::Note(_) => top += CAPTION_H,
        }
    }
    None
}

/// Header icon buttons (refresh, gear) in flyout DIP coords.
pub fn fly_btns() -> (D2D_RECT_F, D2D_RECT_F) {
    let top = PAD + (TITLE_H - BTN) / 2.0;
    let gear = rect(FLYOUT_W - PAD - BTN, top, FLYOUT_W - PAD, top + BTN);
    let refresh = rect(gear.left - 4.0 - BTN, top, gear.left - 4.0, top + BTN);
    (refresh, gear)
}

// ---------- settings layout (DIP) ----------

pub const SET_W: f32 = 400.0;
const SET_PAD: f32 = 24.0;
const CARD_H: f32 = 56.0;
const CARD_GAP: f32 = 4.0;
pub const N_CARDS: usize = 16;
pub const CARD_ACCOUNT: usize = 0;
pub const CARD_CAPS: usize = 1;
pub const CARD_AUTOSTART: usize = 2;
pub const CARD_CODEX: usize = 3;
pub const CARD_ALERTS: usize = 4;
pub const CARD_UPDATE_CHECKS: usize = 5;
pub const UPDATE_CHECKS_LABEL: &str = "Automatically check for updates";
pub const CARD_LID: usize = 6;
/// Card index of the auto-refresh interval row (pills, ←/→ keyboard handling).
pub const CARD_INTERVAL: usize = 7;
pub const CARD_REFRESH: usize = 8;
pub const CARD_ABOUT: usize = 9;
pub const CARD_QUIT: usize = 10;
pub const CARD_DIAGNOSTICS: usize = 11;
pub const CARD_CODEX_SERVER: usize = 12;
pub const CARD_PACE: usize = 13;
pub const CARD_QUOTA_DISPLAY: usize = 14;
pub const CARD_RESET_FORMAT: usize = 15;
const DIAGNOSTICS_H: f32 = 720.0;

pub fn settings_height() -> f32 {
    let cards = N_CARDS as f32 * CARD_H + (N_CARDS as f32 - 1.0) * CARD_GAP;
    SET_PAD + cards + DIAGNOSTICS_H - CARD_H + SET_PAD
}

pub fn settings_rects(scroll: f32) -> [D2D_RECT_F; N_CARDS] {
    let mut out = [rect(0.0, 0.0, 0.0, 0.0); N_CARDS];
    let mut y = SET_PAD - scroll;
    for (index, r) in out.iter_mut().enumerate() {
        let height = if index == CARD_DIAGNOSTICS {
            DIAGNOSTICS_H
        } else {
            CARD_H
        };
        *r = rect(SET_PAD, y, SET_W - SET_PAD, y + height);
        y += height + CARD_GAP;
    }
    out
}

/// Interval pill rects, right-aligned inside the auto-refresh card.
pub fn interval_pills(card: &D2D_RECT_F) -> [D2D_RECT_F; 4] {
    let pw = 40.0;
    let ph = 24.0;
    let gap = 4.0;
    let cy = (card.top + card.bottom) / 2.0;
    let mut out = [rect(0.0, 0.0, 0.0, 0.0); 4];
    let mut right = card.right - 16.0;
    for i in (0..4).rev() {
        out[i] = rect(right - pw, cy - ph / 2.0, right, cy + ph / 2.0);
        right -= pw + gap;
    }
    out
}

// ---------- cached brushes ----------

struct BrushCache {
    key: (bool, (u8, u8, u8), Option<util::ContrastColors>),
    text: ID2D1SolidColorBrush,
    dim: ID2D1SolidColorBrush,
    track: ID2D1SolidColorBrush,
    divider: ID2D1SolidColorBrush,
    stroke: ID2D1SolidColorBrush,
    card_bg: ID2D1SolidColorBrush,
    card_hover: ID2D1SolidColorBrush,
    card_stroke: ID2D1SolidColorBrush,
    control_fill: ID2D1SolidColorBrush,
    control_hover: ID2D1SolidColorBrush,
    control_stroke: ID2D1SolidColorBrush,
    strong_stroke: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    amber: ID2D1SolidColorBrush,
    red: ID2D1SolidColorBrush,
    white: ID2D1SolidColorBrush,
}

const AMBER: (u8, u8, u8) = (255, 185, 0);
const RED: (u8, u8, u8) = (232, 17, 35);

// ---------- gfx stack ----------

pub struct Surface {
    swap: IDXGISwapChain1,
    dc: ID2D1DeviceContext,
    rt: ID2D1RenderTarget,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    target_bmp: Option<ID2D1Bitmap1>,
    fmt_body: IDWriteTextFormat,
    fmt_body_sb: IDWriteTextFormat,
    fmt_body_1: IDWriteTextFormat,
    fmt_caption: IDWriteTextFormat,
    fmt_caption_1: IDWriteTextFormat,
    fmt_glyph: IDWriteTextFormat,
    fmt_glyph_lg: IDWriteTextFormat,
    _ellipsis: IDWriteInlineObject,
    _body_ellipsis: IDWriteInlineObject,
    brushes: Option<BrushCache>,
    w: u32,
    h: u32,
}

impl Surface {
    #[inline(never)]
    fn create_formats(dwrite: &IDWriteFactory) -> Result<[IDWriteTextFormat; 7]> {
        unsafe {
            let mut formats = Vec::with_capacity(7);
            for (family, size, weight) in [
                (
                    w!("Segoe UI Variable Text"),
                    SIZE_BODY,
                    DWRITE_FONT_WEIGHT_NORMAL,
                ),
                (
                    w!("Segoe UI Variable Text"),
                    SIZE_BODY,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                ),
                (
                    w!("Segoe UI Variable Text"),
                    SIZE_BODY,
                    DWRITE_FONT_WEIGHT_NORMAL,
                ),
                (
                    w!("Segoe UI Variable Small"),
                    SIZE_CAPTION,
                    DWRITE_FONT_WEIGHT_NORMAL,
                ),
                (
                    w!("Segoe UI Variable Small"),
                    SIZE_CAPTION,
                    DWRITE_FONT_WEIGHT_NORMAL,
                ),
                (w!("Segoe Fluent Icons"), 13.0, DWRITE_FONT_WEIGHT_NORMAL),
                (w!("Segoe Fluent Icons"), 16.0, DWRITE_FONT_WEIGHT_NORMAL),
            ] {
                formats.push(dwrite.CreateTextFormat(
                    family,
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("en-us"),
                )?);
            }

            formats.try_into().map_err(|_| Error::from(E_UNEXPECTED))
        }
    }

    pub fn new(hwnd: HWND) -> Result<Self> {
        unsafe {
            // WARP, not hardware: the HW driver's user-mode heaps cost ~40 MB
            // private and survive device release. WARP rasterizes our tiny
            // ~330px surface on CPU in microseconds, DWM still composes the
            // result on the GPU, and private RAM stays low.
            let mut d3d: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )?;
            let d3d = d3d.unwrap();
            let dxgi_dev: IDXGIDevice = d3d.cast()?;
            let adapter = dxgi_dev.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;

            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: 8,
                Height: 8,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swap = factory.CreateSwapChainForComposition(&d3d, &desc, None)?;

            let d2df: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2ddev = d2df.CreateDevice(&dxgi_dev)?;
            let dc = d2ddev.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let rt: ID2D1RenderTarget = dc.cast()?; // single QI, reused for brush creation

            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi_dev)?;
            let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&swap)?;
            target.SetRoot(&visual)?;
            dcomp.Commit()?;

            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let [fmt_body, fmt_body_sb, fmt_body_1, fmt_caption, fmt_caption_1, fmt_glyph, fmt_glyph_lg] =
                Self::create_formats(&dwrite)?;
            let mut signs = Vec::with_capacity(2);
            for format in [&fmt_body_1, &fmt_caption_1] {
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                let sign = dwrite.CreateEllipsisTrimmingSign(format)?;
                format.SetTrimming(
                    &DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    },
                    &sign,
                )?;
                signs.push(sign);
            }
            let [body_ellipsis, ellipsis]: [_; 2] =
                signs.try_into().map_err(|_| Error::from(E_UNEXPECTED))?;
            fmt_caption.SetTrimming(
                &DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                },
                &ellipsis,
            )?;

            Ok(Self {
                swap,
                dc,
                rt,
                _dcomp: dcomp,
                _target: target,
                _visual: visual,
                target_bmp: None,
                fmt_body,
                fmt_body_sb,
                fmt_body_1,
                fmt_caption,
                fmt_caption_1,
                fmt_glyph,
                fmt_glyph_lg,
                _ellipsis: ellipsis,
                _body_ellipsis: body_ellipsis,
                brushes: None,
                w: 0,
                h: 0,
            })
        }
    }

    fn ensure_size(&mut self, w: u32, h: u32, dpi: f32) -> Result<()> {
        unsafe {
            if self.w != w || self.h != h {
                self.dc.SetTarget(None);
                self.target_bmp = None;
                self.swap.ResizeBuffers(
                    2,
                    w,
                    h,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )?;
                self.w = w;
                self.h = h;
            }
            if self.target_bmp.is_none() {
                let surface: IDXGISurface = self.swap.GetBuffer(0)?;
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: dpi,
                    dpiY: dpi,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                    colorContext: std::mem::ManuallyDrop::new(None),
                };
                let bmp = self
                    .dc
                    .CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
                self.dc.SetTarget(&bmp);
                self.target_bmp = Some(bmp);
            }
            self.dc.SetDpi(dpi, dpi);
            Ok(())
        }
    }

    fn ensure_brushes(
        &mut self,
        dark: bool,
        accent: (u8, u8, u8),
        contrast: Option<util::ContrastColors>,
    ) -> Result<()> {
        let key = (dark, accent, contrast);
        if self.brushes.as_ref().map(|b| b.key) == Some(key) {
            return Ok(());
        }
        let p = Palette::new(dark, contrast);
        let accent_rgb = contrast.map_or(accent, |colors| colors.accent);
        let accent_text = contrast.map_or((255, 255, 255), |colors| colors.accent_text);
        let colors = [
            p.text,
            p.dim,
            p.track,
            p.divider,
            p.stroke,
            p.card_bg,
            p.card_hover,
            p.card_stroke,
            p.control_fill,
            p.control_hover,
            p.control_stroke,
            p.strong_stroke,
            col_rgb(accent_rgb, 1.0),
            col_rgb(AMBER, 1.0),
            col_rgb(RED, 1.0),
            col_rgb(accent_text, 1.0),
        ];
        let [text, dim, track, divider, stroke, card_bg, card_hover, card_stroke, control_fill, control_hover, control_stroke, strong_stroke, accent, amber, red, white] =
            Self::create_brushes(&self.rt, colors)?;
        self.brushes = Some(BrushCache {
            key,
            text,
            dim,
            track,
            divider,
            stroke,
            card_bg,
            card_hover,
            card_stroke,
            control_fill,
            control_hover,
            control_stroke,
            strong_stroke,
            accent,
            amber,
            red,
            white,
        });
        Ok(())
    }

    #[inline(never)]
    fn create_brushes(
        rt: &ID2D1RenderTarget,
        colors: [D2D1_COLOR_F; 16],
    ) -> Result<[ID2D1SolidColorBrush; 16]> {
        let mut brushes = Vec::with_capacity(colors.len());
        for color in colors {
            brushes.push(unsafe { rt.CreateSolidColorBrush(&color, None)? });
        }
        brushes.try_into().map_err(|_| Error::from(E_UNEXPECTED))
    }

    fn cache(&self) -> &BrushCache {
        self.brushes.as_ref().expect("ensure_brushes called first")
    }

    /// severity → cached fill brush (accent / amber / red)
    fn sev_brush<'a>(
        &'a self,
        severity: &Option<ProviderSeverity>,
        percent: f64,
    ) -> &'a ID2D1SolidColorBrush {
        let b = self.cache();
        if b.key.2.is_some() {
            return &b.accent;
        }
        match util::severity_rgb(severity, percent, b.key.1) {
            AMBER => &b.amber,
            RED => &b.red,
            _ => &b.accent,
        }
    }

    // ---------- flyout ----------

    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    pub fn render_flyout(
        &mut self,
        w_px: u32,
        h_px: u32,
        dpi: f32,
        view: &View,
        dark: bool,
        accent: (u8, u8, u8),
        contrast: Option<util::ContrastColors>,
        scroll: f32,
        hover: FlyHover,
        focus: i32,
        fetching: bool,
        update_dot: bool,
        vibe_on: bool,
        vibe_caption: &str,
    ) -> Result<()> {
        self.ensure_size(w_px.max(8), h_px.max(8), dpi)?;
        self.ensure_brushes(dark, accent, contrast)?;
        unsafe {
            let background = contrast.map(|colors| col_rgb(colors.background, 1.0));
            self.dc.BeginDraw();
            self.dc
                .Clear(background.as_ref().map(|color| color as *const _));

            let w_dip = FLYOUT_W;
            let h_dip = h_px as f32 / (dpi / 96.0);
            let translated = Matrix3x2 {
                M11: 1.0,
                M22: 1.0,
                M32: -scroll,
                ..Default::default()
            };
            self.dc.SetTransform(&translated);
            match view {
                View::Loading => self.draw_message(w_dip, "Loading usage…", None)?,
                View::Error(msg) => {
                    let mut lines = msg.splitn(2, '\n');
                    let head = lines.next().unwrap_or("Can't load usage");
                    let rest = lines.next();
                    self.draw_message(w_dip, head, rest)?
                }
                View::Data(d) => self.draw_data(w_dip, d)?,
            }

            let vibe = vibe_row(view);
            self.draw_vibe_row(
                vibe,
                vibe_on,
                vibe_caption,
                hover == FlyHover::Vibe,
                focus == 2,
            )?;
            self.draw_footer(view, hover == FlyHover::Footer, focus == 3)?;

            self.draw_header_buttons(hover, focus, fetching, update_dot)?;

            let identity = Matrix3x2 {
                M11: 1.0,
                M22: 1.0,
                ..Default::default()
            };
            self.dc.SetTransform(&identity);
            let content_h = flyout_height(view);
            if content_h > h_dip {
                let b = self.cache();
                let track_h = h_dip - 2.0 * PAD;
                let thumb_h = (track_h * h_dip / content_h).max(28.0);
                let thumb_y = PAD + (track_h - thumb_h) * (scroll / (content_h - h_dip));
                self.rounded(
                    rect(w_dip - 5.0, thumb_y, w_dip - 2.0, thumb_y + thumb_h),
                    1.5,
                    &b.dim,
                )?;
            }

            // 1px flyout surface stroke inside the DWM rounded corners
            let rr = D2D1_ROUNDED_RECT {
                rect: rect(0.5, 0.5, w_dip - 0.5, h_dip - 0.5),
                radiusX: 7.5,
                radiusY: 7.5,
            };
            self.dc
                .DrawRoundedRectangle(&rr, &self.cache().stroke, 1.0, None);

            self.dc.EndDraw(None, None)?;
            self.swap.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    fn draw_header_buttons(
        &self,
        hover: FlyHover,
        focus: i32,
        fetching: bool,
        update_dot: bool,
    ) -> Result<()> {
        let (r_refresh, r_gear) = fly_btns();
        let b = self.cache();
        if hover == FlyHover::Refresh {
            self.rounded(r_refresh, 4.0, &b.control_hover)?;
        }
        if hover == FlyHover::Gear {
            self.rounded(r_gear, 4.0, &b.control_hover)?;
        }
        let refresh_brush = if fetching { &b.dim } else { &b.text };
        self.glyph("\u{E72C}", r_refresh, refresh_brush)?; // Refresh (dim = in flight)
        self.glyph("\u{E713}", r_gear, &b.text)?; // Settings
        if update_dot {
            // an update waits behind the gear — quiet accent dot, no nag
            let e = D2D1_ELLIPSE {
                point: D2D_POINT_2F {
                    x: r_gear.right - 5.0,
                    y: r_gear.top + 5.0,
                },
                radiusX: 3.0,
                radiusY: 3.0,
            };
            unsafe { self.dc.FillEllipse(&e, &b.accent) };
        }
        match focus {
            0 => self.focus_ring(r_refresh, 4.0)?,
            1 => self.focus_ring(r_gear, 4.0)?,
            _ => {}
        }
        Ok(())
    }

    fn draw_data(&self, w: f32, d: &FlyoutData) -> Result<()> {
        let b = self.cache();

        let mut y = PAD;
        for (si, sec) in d.sections.iter().enumerate() {
            if si > 0 {
                y += SEC_GAP;
                self.fill(rect(PAD, y, w - PAD, y + 1.0), &b.divider);
                y += 1.0 + SEC_GAP;
            }

            // header: provider name left, plan right — the first section's
            // header shares its row with the refresh/gear buttons
            let plan_right = if si == 0 {
                fly_btns().0.left - GAP
            } else {
                w - PAD
            };
            self.text(
                sec.title,
                &self.fmt_body_sb,
                rect(PAD, y, plan_right, y + TITLE_H),
                &b.text,
                false,
            )?;
            if !sec.plan.is_empty() {
                self.text(
                    &sec.plan,
                    &self.fmt_caption_1,
                    rect(PAD, y + 2.0, plan_right, y + 2.0 + CAPTION_H),
                    &b.dim,
                    true,
                )?;
            }
            y += TITLE_H + SECTION_GAP;

            if let Some(status) = &sec.status {
                self.text(
                    status,
                    &self.fmt_caption_1,
                    rect(PAD, y, w - PAD, y + CAPTION_H),
                    &b.dim,
                    false,
                )?;
                y += CAPTION_H + GAP;
            }

            match &sec.body {
                SectionBody::Rows(rows) => {
                    for (i, row) in rows.iter().enumerate() {
                        if i > 0 {
                            y += ROW_GAP;
                        }
                        let fill = self.sev_brush(
                            &row.pace.severity(row.severity),
                            if matches!(row.pace, Pace::Level) {
                                row.percent
                            } else {
                                0.0
                            },
                        );
                        self.text(
                            &row.label,
                            if matches!(row.pace, Pace::Level | Pace::OnTrack) {
                                &self.fmt_body_1
                            } else {
                                &self.fmt_caption_1
                            },
                            rect(PAD, y, w - PAD, y + LABEL_H),
                            &b.text,
                            false,
                        )?;

                        let bar_y = y + LABEL_H + GAP;
                        let bar_w = w - 2.0 * PAD;
                        self.rounded(
                            rect(PAD, bar_y, PAD + bar_w, bar_y + BAR_H),
                            BAR_H / 2.0,
                            &b.track,
                        )?;
                        let frac = (row.percent / 100.0).clamp(0.0, 1.0) as f32;
                        if frac > 0.005 {
                            let fw = (bar_w * frac).max(BAR_H);
                            self.rounded(
                                rect(PAD, bar_y, PAD + fw, bar_y + BAR_H),
                                BAR_H / 2.0,
                                fill,
                            )?;
                        }

                        if let Some(even) = row.pace.even_fraction() {
                            let x = PAD + bar_w * even;
                            self.fill(
                                rect(x - 0.5, bar_y - 2.0, x + 0.5, bar_y + BAR_H + 2.0),
                                &b.text,
                            );
                        }

                        let cap_y = bar_y + BAR_H + GAP;
                        self.text(
                            &quota_value(row.percent),
                            &self.fmt_caption_1,
                            rect(PAD, cap_y, PAD + 96.0, cap_y + CAPTION_H),
                            &b.text,
                            false,
                        )?;
                        if !row.reset_text.is_empty() {
                            self.text(
                                &row.reset_text,
                                &self.fmt_caption_1,
                                rect(PAD + 96.0, cap_y, w - PAD, cap_y + CAPTION_H),
                                &b.dim,
                                true,
                            )?;
                        }
                        y += ROW_BLOCK;
                    }
                }
                SectionBody::Note(msg) => {
                    self.text(
                        msg,
                        &self.fmt_caption_1,
                        rect(PAD, y, w - PAD, y + CAPTION_H),
                        &b.dim,
                        false,
                    )?;
                    y += CAPTION_H;
                }
            }
        }

        Ok(())
    }

    fn draw_footer(&self, view: &View, hover: bool, focused: bool) -> Result<()> {
        let b = self.cache();
        let r = footer_action_rect(view);
        let div_y = r.top - GAP - CAPTION_H - FOOTER_GAP_BELOW - 1.0;
        self.fill(rect(PAD, div_y, FLYOUT_W - PAD, div_y + 1.0), &b.divider);
        self.text(
            &updated_caption(view),
            &self.fmt_caption_1,
            rect(PAD, r.top - CAPTION_H - GAP, FLYOUT_W - PAD, r.top - GAP),
            &b.dim,
            false,
        )?;
        if hover {
            self.rounded(r, 4.0, &b.control_hover)?;
        }
        self.text(
            &footer_action(
                view,
                crate::provider::state::SystemClock.read().unix_seconds,
            ),
            &self.fmt_caption_1,
            r,
            &b.text,
            false,
        )?;
        if focused {
            self.focus_ring(r, 4.0)?;
        }
        Ok(())
    }

    /// Vibecode wake-lock row — settings-style card with a Fluent ToggleSwitch.
    fn draw_vibe_row(
        &self,
        r: D2D_RECT_F,
        on: bool,
        caption: &str,
        hover: bool,
        focused: bool,
    ) -> Result<()> {
        let b = self.cache();
        let bg = if hover { &b.card_hover } else { &b.card_bg };
        self.rounded(r, 4.0, bg)?;
        let rr = D2D1_ROUNDED_RECT {
            rect: rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5),
            radiusX: 3.5,
            radiusY: 3.5,
        };
        let border = if hover && b.key.2.is_some() {
            &b.accent
        } else {
            &b.card_stroke
        };
        unsafe { self.dc.DrawRoundedRectangle(&rr, border, 1.0, None) };

        let cy = (r.top + r.bottom) / 2.0;
        let icon_brush = if on { &b.accent } else { &b.text };
        self.icon16(
            "\u{E945}",
            rect(r.left + 12.0, cy - 10.0, r.left + 32.0, cy + 10.0),
            icon_brush,
        )?;

        let text_left = r.left + 40.0;
        let text_right = r.right - 56.0; // clear of the 40px toggle + margin
        self.text(
            "Keep computer awake",
            &self.fmt_body_1,
            rect(text_left, r.top + 4.0, text_right, r.top + 4.0 + LABEL_H),
            &b.text,
            false,
        )?;
        self.text(
            caption,
            &self.fmt_caption,
            rect(
                text_left,
                r.top + 24.0,
                text_right,
                r.top + 24.0 + 2.0 * CAPTION_H,
            ),
            &b.dim,
            false,
        )?;

        self.toggle(r.right - 12.0, cy, on)?;
        if focused {
            self.focus_ring(r, 4.0)?;
        }
        Ok(())
    }

    fn draw_message(&self, w: f32, head: &str, body: Option<&str>) -> Result<()> {
        let b = self.cache();
        self.text(
            head,
            &self.fmt_body_sb,
            rect(PAD, PAD + 8.0, w - PAD, PAD + 28.0),
            &b.text,
            false,
        )?;
        if let Some(t) = body {
            self.text(
                t,
                &self.fmt_caption,
                rect(PAD, PAD + 36.0, w - PAD, 108.0),
                &b.dim,
                false,
            )?;
        }
        Ok(())
    }

    // ---------- settings ----------

    #[allow(clippy::too_many_arguments)]
    pub fn render_settings(
        &mut self,
        w_px: u32,
        h_px: u32,
        dpi: f32,
        st: &SettingsView,
        dark: bool,
        accent: (u8, u8, u8),
        contrast: Option<util::ContrastColors>,
        scroll: f32,
    ) -> Result<()> {
        self.ensure_size(w_px.max(8), h_px.max(8), dpi)?;
        self.ensure_brushes(dark, accent, contrast)?;
        unsafe {
            let background = contrast.map(|colors| col_rgb(colors.background, 1.0));
            self.dc.BeginDraw();
            self.dc
                .Clear(background.as_ref().map(|color| color as *const _));

            let labels: [&str; N_CARDS] = [
                "Claude account",
                "Caps Lock status light",
                "Start with Windows",
                "Show Codex usage",
                "Alert at 75% usage",
                UPDATE_CHECKS_LABEL,
                st.lid_label.as_str(),
                "Auto-refresh",
                st.refresh_label.as_str(),
                st.about.as_str(),
                "Quit Claudometer",
                "Diagnostics",
                "Codex app-server",
                "Color bars by current pace",
                "Quota display",
                "Reset format",
            ];
            // Segoe Fluent Icons: account, keyboard, power, command prompt,
            // bell (EA8F Ringer — E7ED is the muted bell), download, clock,
            // refresh, info, cancel
            let icons = [
                "\u{E77B}", "\u{E765}", "\u{E7E8}", "\u{E756}", "\u{EA8F}", "\u{E895}", "\u{E7BA}",
                "\u{E823}", "\u{E72C}", "\u{E946}", "\u{E711}", "\u{E9D9}", "\u{E756}", "\u{E9D9}",
                "\u{E9D9}", "\u{E823}",
            ];
            let cards = settings_rects(scroll);
            for (i, card) in cards.iter().enumerate() {
                let b = self.cache();
                let bg = if st.hover == i as i32 {
                    &b.card_hover
                } else {
                    &b.card_bg
                };
                self.rounded(*card, 4.0, bg)?;
                let rr = D2D1_ROUNDED_RECT {
                    rect: rect(
                        card.left + 0.5,
                        card.top + 0.5,
                        card.right - 0.5,
                        card.bottom - 0.5,
                    ),
                    radiusX: 3.5,
                    radiusY: 3.5,
                };
                let border = if contrast.is_some() && st.hover == i as i32 {
                    &b.accent
                } else {
                    &b.card_stroke
                };
                self.dc.DrawRoundedRectangle(&rr, border, 1.0, None);

                if i == CARD_DIAGNOSTICS {
                    self.text(
                        "Diagnostics",
                        &self.fmt_body_sb,
                        rect(
                            card.left + 16.0,
                            card.top + 16.0,
                            card.right - 100.0,
                            card.top + 36.0,
                        ),
                        &b.text,
                        false,
                    )?;
                    self.button(card.right - 16.0, card.top + 26.0, st.diagnostics_copy)?;
                    self.text(
                        &st.diagnostics,
                        &self.fmt_caption,
                        rect(
                            card.left + 16.0,
                            card.top + 52.0,
                            card.right - 16.0,
                            card.bottom - 16.0,
                        ),
                        &b.dim,
                        false,
                    )?;
                    if st.focus == i as i32 {
                        self.focus_ring(*card, 4.0)?;
                    }
                    continue;
                }

                let cy0 = (card.top + card.bottom) / 2.0;
                let icon_brush = if (i == CARD_ACCOUNT && st.account_connected)
                    || (i == CARD_ABOUT && st.update_ready)
                {
                    &b.accent
                } else {
                    &b.text
                };
                self.icon16(
                    icons[i],
                    rect(card.left + 16.0, cy0 - 10.0, card.left + 36.0, cy0 + 10.0),
                    icon_brush,
                )?;
                let label_right = if i == CARD_INTERVAL {
                    card.right - 200.0
                } else {
                    card.right - 120.0
                };
                if i == CARD_ACCOUNT || i == CARD_CAPS || i == CARD_LID {
                    self.text(
                        labels[i],
                        &self.fmt_body,
                        rect(
                            card.left + 48.0,
                            card.top + 7.0,
                            label_right,
                            card.top + 27.0,
                        ),
                        &b.text,
                        false,
                    )?;
                    self.text(
                        match i {
                            CARD_ACCOUNT => &st.account_caption,
                            CARD_CAPS => &st.caps_caption,
                            _ => &st.lid_caption,
                        },
                        &self.fmt_caption_1,
                        rect(
                            card.left + 48.0,
                            card.top + 29.0,
                            label_right,
                            card.top + 45.0,
                        ),
                        &b.dim,
                        false,
                    )?;
                } else {
                    self.text_v(
                        labels[i],
                        &self.fmt_body,
                        rect(card.left + 48.0, card.top, label_right, card.bottom),
                        &b.text,
                    )?;
                }

                let cy = (card.top + card.bottom) / 2.0;
                if let Some(on) = st.toggle_for(i) {
                    self.toggle(card.right - 16.0, cy, on)?;
                } else {
                    let label = match i {
                        CARD_ACCOUNT => Some(st.account_action),
                        CARD_CAPS => match st.caps_control {
                            CapsControl::Unavailable => None,
                            CapsControl::Toggle(on) => {
                                self.toggle(card.right - 16.0, cy, on)?;
                                None
                            }
                            CapsControl::Retry => Some("Retry"),
                        },
                        CARD_LID => {
                            if let Some(action) = st.lid_action {
                                Some(action)
                            } else {
                                self.toggle(card.right - 16.0, cy, st.lid_on)?;
                                None
                            }
                        }
                        CARD_INTERVAL => {
                            self.interval_row(card, st.poll_secs)?;
                            None
                        }
                        CARD_REFRESH => Some("Refresh"),
                        CARD_ABOUT => Some(st.about_btn),
                        CARD_QUIT => Some("Quit"),
                        CARD_QUOTA_DISPLAY | CARD_RESET_FORMAT => Some(st.row_choice(i)),
                        _ => None,
                    };
                    if let Some(label) = label {
                        self.button(card.right - 16.0, cy, label)?;
                    }
                }

                if st.focus == i as i32 {
                    self.focus_ring(*card, 4.0)?;
                }
            }

            let viewport_h = h_px as f32 / (dpi / 96.0);
            if settings_height() > viewport_h {
                let b = self.cache();
                let track_h = viewport_h - 2.0 * SET_PAD;
                let thumb_h = (track_h * viewport_h / settings_height()).max(32.0);
                let max_scroll = settings_height() - viewport_h;
                let thumb_y = SET_PAD + (track_h - thumb_h) * (scroll / max_scroll);
                self.rounded(
                    rect(SET_W - 7.0, thumb_y, SET_W - 4.0, thumb_y + thumb_h),
                    1.5,
                    &b.dim,
                )?;
            }

            self.dc.EndDraw(None, None)?;
            self.swap.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    /// Segmented interval pills (SelectorBar-style), selected = accent
    fn interval_row(&self, card: &D2D_RECT_F, poll_secs: u32) -> Result<()> {
        unsafe {
            let pills = interval_pills(card);
            let b = self.cache();
            let f = &self.fmt_caption;
            for (i, pill) in pills.iter().enumerate() {
                let (secs, label) = INTERVALS[i];
                let selected = secs == poll_secs;
                if selected {
                    self.rounded(*pill, 12.0, &b.accent)?;
                } else {
                    self.rounded(*pill, 12.0, &b.control_fill)?;
                    let rr = D2D1_ROUNDED_RECT {
                        rect: rect(
                            pill.left + 0.5,
                            pill.top + 0.5,
                            pill.right - 0.5,
                            pill.bottom - 0.5,
                        ),
                        radiusX: 11.5,
                        radiusY: 11.5,
                    };
                    self.dc
                        .DrawRoundedRectangle(&rr, &b.control_stroke, 1.0, None);
                }
                let brush = if selected { &b.white } else { &b.text };
                self.draw_text(
                    label,
                    f,
                    *pill,
                    brush,
                    DWRITE_TEXT_ALIGNMENT_CENTER,
                    DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                )?;
            }
            Ok(())
        }
    }

    /// Fluent ToggleSwitch, right-aligned at (right_edge, cy)
    fn toggle(&self, right_edge: f32, cy: f32, on: bool) -> Result<()> {
        unsafe {
            let b = self.cache();
            let w = 40.0;
            let h = 20.0;
            let r = rect(right_edge - w, cy - h / 2.0, right_edge, cy + h / 2.0);
            if on {
                self.rounded(r, h / 2.0, &b.accent)?;
                let e = D2D1_ELLIPSE {
                    point: D2D_POINT_2F {
                        x: r.right - 10.0,
                        y: cy,
                    },
                    radiusX: 7.0,
                    radiusY: 7.0,
                };
                self.dc.FillEllipse(&e, &b.white);
            } else {
                let rr = D2D1_ROUNDED_RECT {
                    rect: r,
                    radiusX: h / 2.0,
                    radiusY: h / 2.0,
                };
                self.dc
                    .DrawRoundedRectangle(&rr, &b.strong_stroke, 1.0, None);
                let e = D2D1_ELLIPSE {
                    point: D2D_POINT_2F {
                        x: r.left + 10.0,
                        y: cy,
                    },
                    radiusX: 6.0,
                    radiusY: 6.0,
                };
                self.dc.FillEllipse(&e, &b.strong_stroke);
            }
            Ok(())
        }
    }

    /// Fluent standard button, right-aligned at (right_edge, cy)
    fn button(&self, right_edge: f32, cy: f32, label: &str) -> Result<()> {
        unsafe {
            let b = self.cache();
            let w = 84.0;
            let h = 32.0;
            let r = rect(right_edge - w, cy - h / 2.0, right_edge, cy + h / 2.0);
            self.rounded(r, 4.0, &b.control_fill)?;
            let rr = D2D1_ROUNDED_RECT {
                rect: rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5),
                radiusX: 3.5,
                radiusY: 3.5,
            };
            self.dc
                .DrawRoundedRectangle(&rr, &b.control_stroke, 1.0, None);
            self.glyph_with(&self.fmt_body, label, r, &b.text)
        }
    }

    // ---------- primitives ----------

    /// Keyboard focus visual: 2px accent outline just outside the target.
    fn focus_ring(&self, r: D2D_RECT_F, radius: f32) -> Result<()> {
        unsafe {
            let rr = D2D1_ROUNDED_RECT {
                rect: rect(r.left - 3.0, r.top - 3.0, r.right + 3.0, r.bottom + 3.0),
                radiusX: radius + 3.0,
                radiusY: radius + 3.0,
            };
            self.dc
                .DrawRoundedRectangle(&rr, &self.cache().accent, 2.0, None);
            Ok(())
        }
    }

    fn glyph(&self, s: &str, r: D2D_RECT_F, brush: &ID2D1SolidColorBrush) -> Result<()> {
        self.glyph_with(&self.fmt_glyph, s, r, brush)
    }

    /// 16px leading icon for settings cards (Win11 Settings row style).
    fn icon16(&self, s: &str, r: D2D_RECT_F, brush: &ID2D1SolidColorBrush) -> Result<()> {
        self.glyph_with(&self.fmt_glyph_lg, s, r, brush)
    }

    fn glyph_with(
        &self,
        f: &IDWriteTextFormat,
        s: &str,
        r: D2D_RECT_F,
        brush: &ID2D1SolidColorBrush,
    ) -> Result<()> {
        self.draw_text(
            s,
            f,
            r,
            brush,
            DWRITE_TEXT_ALIGNMENT_CENTER,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        )
    }

    fn text(
        &self,
        s: &str,
        f: &IDWriteTextFormat,
        r: D2D_RECT_F,
        brush: &ID2D1SolidColorBrush,
        trailing: bool,
    ) -> Result<()> {
        self.draw_text(
            s,
            f,
            r,
            brush,
            if trailing {
                DWRITE_TEXT_ALIGNMENT_TRAILING
            } else {
                DWRITE_TEXT_ALIGNMENT_LEADING
            },
            DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
        )
    }

    /// vertically-centered text
    fn text_v(
        &self,
        s: &str,
        f: &IDWriteTextFormat,
        r: D2D_RECT_F,
        brush: &ID2D1SolidColorBrush,
    ) -> Result<()> {
        self.draw_text(
            s,
            f,
            r,
            brush,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        )
    }

    #[inline(never)]
    fn draw_text(
        &self,
        s: &str,
        f: &IDWriteTextFormat,
        r: D2D_RECT_F,
        brush: &ID2D1SolidColorBrush,
        alignment: DWRITE_TEXT_ALIGNMENT,
        paragraph: DWRITE_PARAGRAPH_ALIGNMENT,
    ) -> Result<()> {
        unsafe {
            f.SetTextAlignment(alignment)?;
            f.SetParagraphAlignment(paragraph)?;
            let wide: Vec<u16> = s.encode_utf16().collect();
            self.dc.DrawText(
                &wide,
                f,
                &r,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            Ok(())
        }
    }

    fn rounded(&self, r: D2D_RECT_F, radius: f32, brush: &ID2D1SolidColorBrush) -> Result<()> {
        unsafe {
            let rr = D2D1_ROUNDED_RECT {
                rect: r,
                radiusX: radius,
                radiusY: radius,
            };
            self.dc.FillRoundedRectangle(&rr, brush);
            Ok(())
        }
    }

    fn fill(&self, r: D2D_RECT_F, brush: &ID2D1SolidColorBrush) {
        unsafe { self.dc.FillRectangle(&r, brush) }
    }
}

// ---------- palette (Fluent theme tokens, hand-translated for D2D) ----------

struct Palette {
    text: D2D1_COLOR_F,
    dim: D2D1_COLOR_F,
    track: D2D1_COLOR_F,
    divider: D2D1_COLOR_F,
    stroke: D2D1_COLOR_F,
    card_bg: D2D1_COLOR_F,
    card_hover: D2D1_COLOR_F,
    card_stroke: D2D1_COLOR_F,
    control_fill: D2D1_COLOR_F,
    control_hover: D2D1_COLOR_F,
    control_stroke: D2D1_COLOR_F,
    strong_stroke: D2D1_COLOR_F,
}

impl Palette {
    fn new(dark: bool, contrast: Option<util::ContrastColors>) -> Self {
        if let Some(colors) = contrast {
            let background = col_rgb(colors.background, 1.0);
            let text = col_rgb(colors.text, 1.0);
            return Self {
                text,
                dim: text,
                track: text,
                divider: text,
                stroke: text,
                card_bg: background,
                card_hover: background,
                card_stroke: text,
                control_fill: background,
                control_hover: background,
                control_stroke: text,
                strong_stroke: text,
            };
        }
        if dark {
            Self {
                text: col(1.0, 1.0, 1.0, 1.0),
                dim: col(1.0, 1.0, 1.0, 0.772),
                track: col(1.0, 1.0, 1.0, 0.16),
                divider: col(1.0, 1.0, 1.0, 0.083),
                stroke: col(0.0, 0.0, 0.0, 0.30),
                card_bg: col(1.0, 1.0, 1.0, 0.054),
                card_hover: col(1.0, 1.0, 1.0, 0.083),
                card_stroke: col(0.0, 0.0, 0.0, 0.10),
                control_fill: col(1.0, 1.0, 1.0, 0.061),
                control_hover: col(1.0, 1.0, 1.0, 0.084),
                control_stroke: col(1.0, 1.0, 1.0, 0.07),
                strong_stroke: col(1.0, 1.0, 1.0, 0.544),
            }
        } else {
            Self {
                text: col(0.0, 0.0, 0.0, 0.894),
                dim: col(0.0, 0.0, 0.0, 0.62),
                track: col(0.0, 0.0, 0.0, 0.14),
                divider: col(0.0, 0.0, 0.0, 0.081),
                stroke: col(0.0, 0.0, 0.0, 0.058),
                card_bg: col(1.0, 1.0, 1.0, 0.70),
                card_hover: col(0.96, 0.96, 0.96, 0.50),
                card_stroke: col(0.0, 0.0, 0.0, 0.058),
                control_fill: col(1.0, 1.0, 1.0, 0.70),
                control_hover: col(0.0, 0.0, 0.0, 0.037),
                control_stroke: col(0.0, 0.0, 0.0, 0.058),
                strong_stroke: col(0.0, 0.0, 0.0, 0.446),
            }
        }
    }
}

fn col(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn col_rgb(rgb: (u8, u8, u8), a: f32) -> D2D1_COLOR_F {
    col(
        rgb.0 as f32 / 255.0,
        rgb.1 as f32 / 255.0,
        rgb.2 as f32 / 255.0,
        a,
    )
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

/// Relative + absolute combined: "just now" → "3m ago" → "at 12:56".
/// Re-rendered on a 30s tick so it never freezes.
fn relative_time(unix: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(unix);
    let diff = (now - unix).max(0);
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else {
        format!("at {}", crate::api::fmt_unix_hhmm(unix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_minutes_deadlines_and_geometry_are_bounded() {
        let mut data = FlyoutData {
            sections: vec![],
            fetched_unix: None,
            note: None,
            next_update_unix: None,
            retry_at_unix: None,
        };
        for (seconds, expected) in [
            (1, "Next update in 1m"),
            (60, "Next update in 1m"),
            (61, "Next update in 2m"),
            (240, "Next update in 4m"),
        ] {
            data.next_update_unix = Some(1000 + seconds);
            assert_eq!(footer_action(&View::Data(data.clone()), 1000), expected);
        }
        data.retry_at_unix = Some(1120);
        assert_eq!(
            footer_action(&View::Data(data.clone()), 1000),
            format!("Retry at {}", crate::api::fmt_unix_hhmm(1120))
        );
        data.next_update_unix = Some(1000);
        assert_eq!(
            footer_action(&View::Data(data.clone()), 1120),
            "Refresh usage now"
        );
        for view in [
            View::Loading,
            View::Error("Offline".into()),
            View::Data(data),
        ] {
            let r = footer_action_rect(&view);
            assert!(r.top > vibe_row(&view).bottom);
            assert_eq!(flyout_height(&view), r.bottom + PAD);
        }
        for (seconds, expected) in [
            (None, "never"),
            (Some(59), "just now"),
            (Some(60), "1m ago"),
            (Some(10800), "3h ago"),
        ] {
            assert_eq!(age_text(seconds), expected);
        }
    }
    #[test]
    fn quota_display_complements_value_without_changing_severity() {
        for (used, expected) in [
            (0.0, "100% left"),
            (48.0, "52% left"),
            (99.5, "0% left"),
            (100.0, "0% left"),
        ] {
            assert_eq!(
                format_quota_value(used, crate::config::QuotaDisplay::Left),
                expected
            );
            assert_eq!(
                format_quota_value(used, crate::config::QuotaDisplay::Used),
                format!("{used:.0}% used")
            );
        }
    }
    #[test]
    fn not_started_is_only_a_claude_session_and_shortcuts_follow_scrolled_row_bounds() {
        use crate::provider::model::{LimitKind, ProviderId};
        for provider in [ProviderId::Claude, ProviderId::Codex] {
            for kind in [LimitKind::Session, LimitKind::Weekly] {
                let limit = UsageLimit::from_adapter(
                    "limit".into(),
                    kind.clone(),
                    "Limit".into(),
                    0.0,
                    None,
                    None,
                    Some(18000),
                )
                .unwrap();
                let row = LimitRow::with_pace(limit, provider, 9000, true);
                let started = provider == ProviderId::Claude && kind == LimitKind::Session;
                assert_eq!(row.reset_text, if started { "Not started" } else { "" });
                let view = View::Data(FlyoutData {
                    sections: vec![Section {
                        help: String::new(),
                        title: if provider == ProviderId::Claude {
                            "Claude"
                        } else {
                            "Codex"
                        },
                        plan: String::new(),
                        status: Some("Outdated".into()),
                        body: SectionBody::Rows(vec![row]),
                    }],
                    fetched_unix: Some(9000),
                    note: None,
                    next_update_unix: None,
                    retry_at_unix: None,
                });
                let rows = accessible_rows(&view);
                assert_eq!(
                    rows[1]
                        .1
                        .contains("The session starts with your first message."),
                    started
                );
                let bounds = rows[1].0;
                assert_eq!(
                    row_shortcut(&view, PAD, bounds.bottom - 1.0),
                    Some(CARD_QUOTA_DISPLAY)
                );
                assert_eq!(
                    row_shortcut(&view, FLYOUT_W - PAD - 1.0, bounds.bottom - 1.0),
                    started.then_some(CARD_RESET_FORMAT)
                );
                assert_eq!(row_shortcut(&view, PAD, bounds.top), None);
                assert_eq!(row_shortcut(&view, PAD - 1.0, bounds.bottom - 1.0), None);
                assert_eq!(row_shortcut(&view, PAD, rows[0].0.bottom - 1.0), None);
            }
        }
    }
    #[test]
    fn pace_presentation_changes_only_flyout_and_toggle_restores_level() {
        let limit = crate::provider::model::UsageLimit::from_adapter(
            "session".into(),
            crate::provider::model::LimitKind::Session,
            "Session".into(),
            60.0,
            None,
            Some(18000),
            Some(18000),
        )
        .unwrap();
        let original = limit.clone();
        let level = LimitRow::with_pace(
            limit.clone(),
            crate::provider::model::ProviderId::Claude,
            9000,
            false,
        );
        assert_eq!(level.pace, Pace::Level);
        assert_eq!(level.label, "Session");
        assert_eq!(level.pace.severity(level.severity), None);
        let projected = LimitRow::with_pace(
            limit.clone(),
            crate::provider::model::ProviderId::Claude,
            9000,
            true,
        );
        let projected_label = projected.label.clone();
        assert!(matches!(projected.pace, Pace::Over { .. }));
        assert!(projected.label.contains(" · Limit "));
        assert_eq!(
            projected.pace.severity(projected.severity),
            Some(ProviderSeverity::Critical)
        );
        assert_eq!(projected.pace.even_fraction(), Some(0.5));
        let view = View::Data(FlyoutData {
            sections: vec![Section {
                title: "Claude",
                plan: String::new(),
                status: None,
                help: String::new(),
                body: SectionBody::Rows(vec![projected]),
            }],
            fetched_unix: Some(9000),
            note: None,
            next_update_unix: None,
            retry_at_unix: None,
        });
        let name = &accessible_rows(&view)[0].1;
        assert!(name.starts_with(&format!("Claude, {projected_label}, 60% used")));
        assert!(name.contains("Over at current pace"));
        assert_eq!(limit.percent, original.percent);
        assert_eq!(limit.severity, original.severity);
        assert_eq!(
            util::severity_rgb(&limit.severity, limit.percent.get(), (1, 2, 3)),
            (1, 2, 3)
        );
        assert_eq!(
            Pace::OnTrack.severity(Some(ProviderSeverity::Critical)),
            Some(ProviderSeverity::Normal)
        );
    }
    #[test]
    fn unprojected_row_preserves_adapter_severity_value_and_missing_reset() {
        let limit = crate::provider::model::UsageLimit::from_adapter(
            "session".into(),
            crate::provider::model::LimitKind::Session,
            "Session".into(),
            64.0,
            Some(crate::provider::model::ProviderSeverity::Warning),
            None,
            Some(18000),
        )
        .unwrap();
        let row = super::LimitRow::from(limit);
        assert_eq!(row.label, "Session");
        assert_eq!(row.percent, 64.0);
        assert_eq!(
            row.severity,
            Some(crate::provider::model::ProviderSeverity::Warning)
        );
        assert!(row.reset_text.is_empty());
    }

    fn settings_view(update_checks_on: bool) -> SettingsView {
        SettingsView {
            diagnostics: String::new(),
            diagnostics_copy: "Copy",
            account_caption: String::new(),
            account_action: "",
            account_connected: false,
            caps_caption: String::new(),
            caps_control: CapsControl::Unavailable,
            autostart: false,
            codex_on: false,
            codex_server_on: false,
            pace_on: true,
            reset_format: crate::config::ResetFormat::Clock,
            quota_display: crate::config::QuotaDisplay::Used,
            alerts_on: false,
            update_checks_on,
            lid_label: String::new(),
            lid_caption: String::new(),
            lid_on: false,
            lid_action: None,
            about: String::new(),
            about_btn: "",
            update_ready: false,
            poll_secs: 60,
            refresh_label: String::new(),
            hover: -1,
            focus: -1,
        }
    }

    #[test]
    fn settings_exposes_update_checks_as_a_toggle_card() {
        assert_eq!(UPDATE_CHECKS_LABEL, "Automatically check for updates");
        assert_eq!(CARD_UPDATE_CHECKS + 1, CARD_LID);
        assert_eq!(
            settings_view(false).toggle_for(CARD_UPDATE_CHECKS),
            Some(false)
        );
        assert_eq!(
            settings_view(true).toggle_for(CARD_UPDATE_CHECKS),
            Some(true)
        );
    }

    #[test]
    fn diagnostics_card_adds_scrollable_bounds_after_existing_controls() {
        let cards = settings_rects(0.0);
        assert_eq!(
            cards[CARD_DIAGNOSTICS].bottom - cards[CARD_DIAGNOSTICS].top,
            DIAGNOSTICS_H
        );
        assert!(cards[CARD_DIAGNOSTICS].top > cards[CARD_QUIT].bottom);
        assert_eq!(settings_height(), cards[CARD_RESET_FORMAT].bottom + SET_PAD);
        assert_eq!(
            settings_rects(100.0)[CARD_DIAGNOSTICS].top,
            cards[CARD_DIAGNOSTICS].top - 100.0
        );
    }

    #[test]
    fn error_status_height_and_accessible_bounds_match_rows() {
        let mut data = FlyoutData {
            fetched_unix: Some(1000),
            note: None,
            next_update_unix: None,
            retry_at_unix: None,
            sections: vec![Section {
                title: "Claude",
                plan: "Synthetic".to_string(),
                status: None,
                help: String::new(),
                body: SectionBody::Rows(vec![LimitRow {
                    label: "Session".to_string(),
                    percent: 50.0,
                    severity: None,
                    reset_text: String::new(),
                    pace: Pace::Level,
                }]),
            }],
        };
        let before = View::Data(data.clone());
        let height = flyout_height(&before);
        let row_top = accessible_rows(&before)[0].0.top;
        data.sections[0].status = Some("Timed out".to_string());
        let after = View::Data(data);
        assert_eq!(flyout_height(&after), height + CAPTION_H + GAP);
        let rows = accessible_rows(&after);
        assert_eq!(rows[0].1, "Claude, Timed out");
        assert_eq!(rows[1].0.top, row_top + CAPTION_H + GAP);
    }
}
