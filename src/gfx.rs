//! Rendering: D3D11 + DXGI composition swapchain (premultiplied alpha)
//! + DirectComposition visual + Direct2D/DirectWrite content.
//! `Surface` is one alpha-composited window canvas — used by both the flyout
//! and the settings window. DWM/accent-policy draws the material behind it.
//!
//! Visuals follow the Fluent type ramp and 4px spacing grid.

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use crate::api::UsageSnapshot;
use crate::util;

#[derive(Clone)]
pub enum View {
    Loading,
    Error(String),
    Data(UsageSnapshot),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FlyHover {
    None,
    Refresh,
    Gear,
}

pub struct SettingsView {
    pub caps_on: bool,
    pub autostart: bool,
    pub poll_secs: u32,
    pub hover: i32, // card index, -1 = none
}

pub const INTERVALS: [(u32, &str); 4] = [(30, "30s"), (60, "1m"), (120, "2m"), (300, "5m")];

// ---------- flyout layout (DIP, 4px grid) ----------

pub const FLYOUT_W: f32 = 328.0;
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

const SIZE_BODY: f32 = 14.0;
const SIZE_CAPTION: f32 = 12.0;

const BTN: f32 = 28.0; // header icon button

pub fn flyout_height(view: &View) -> f32 {
    match view {
        View::Data(s) => {
            let n = s.rows.len().max(1) as f32;
            PAD + TITLE_H
                + SECTION_GAP
                + n * ROW_BLOCK
                + (n - 1.0) * ROW_GAP
                + FOOTER_GAP_ABOVE
                + 1.0
                + FOOTER_GAP_BELOW
                + CAPTION_H
                + PAD
        }
        _ => 120.0,
    }
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
const N_CARDS: usize = 5;

pub fn settings_height() -> f32 {
    let cards = N_CARDS as f32 * CARD_H + (N_CARDS as f32 - 1.0) * CARD_GAP;
    SET_PAD + cards + 12.0 + CAPTION_H + SET_PAD
}

pub fn settings_rects() -> [D2D_RECT_F; N_CARDS] {
    let mut out = [rect(0.0, 0.0, 0.0, 0.0); N_CARDS];
    let mut y = SET_PAD;
    for r in out.iter_mut() {
        *r = rect(SET_PAD, y, SET_W - SET_PAD, y + CARD_H);
        y += CARD_H + CARD_GAP;
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

// ---------- gfx stack ----------

pub struct Surface {
    swap: IDXGISwapChain1,
    dc: ID2D1DeviceContext,
    dwrite: IDWriteFactory,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    target_bmp: Option<ID2D1Bitmap1>,
    w: u32,
    h: u32,
}

impl Surface {
    pub fn new(hwnd: HWND) -> Result<Self> {
        unsafe {
            let mut d3d: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
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
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
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

            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi_dev)?;
            let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&swap)?;
            target.SetRoot(&visual)?;
            dcomp.Commit()?;

            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

            Ok(Self {
                swap,
                dc,
                dwrite,
                _dcomp: dcomp,
                _target: target,
                _visual: visual,
                target_bmp: None,
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
                self.swap
                    .ResizeBuffers(2, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0))?;
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
                let bmp = self.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
                self.dc.SetTarget(&bmp);
                self.target_bmp = Some(bmp);
            }
            self.dc.SetDpi(dpi, dpi);
            Ok(())
        }
    }

    // ---------- flyout ----------

    #[allow(clippy::too_many_arguments)]
    pub fn render_flyout(
        &mut self,
        w_px: u32,
        h_px: u32,
        dpi: f32,
        view: &View,
        dark: bool,
        accent: (u8, u8, u8),
        hover: FlyHover,
        fetching: bool,
        note: Option<&str>,
    ) -> Result<()> {
        self.ensure_size(w_px.max(8), h_px.max(8), dpi)?;
        unsafe {
            self.dc.BeginDraw();
            self.dc.Clear(None);

            let w_dip = FLYOUT_W;
            let h_dip = h_px as f32 / (dpi / 96.0);
            let pal = Palette::new(dark, accent);
            match view {
                View::Loading => self.draw_message(&pal, w_dip, "Loading usage…", None)?,
                View::Error(msg) => {
                    let mut lines = msg.splitn(2, '\n');
                    let head = lines.next().unwrap_or("Can't load usage");
                    let rest = lines.next();
                    self.draw_message(&pal, w_dip, head, rest)?
                }
                View::Data(snap) => self.draw_data(&pal, w_dip, snap, note)?,
            }

            self.draw_header_buttons(&pal, hover, fetching)?;

            // 1px flyout surface stroke inside the DWM rounded corners
            let stroke = self.brush(pal.stroke)?;
            let rr = D2D1_ROUNDED_RECT {
                rect: rect(0.5, 0.5, w_dip - 0.5, h_dip - 0.5),
                radiusX: 7.5,
                radiusY: 7.5,
            };
            self.dc.DrawRoundedRectangle(&rr, &stroke, 1.0, None);

            self.dc.EndDraw(None, None)?;
            self.swap.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    fn draw_header_buttons(&self, pal: &Palette, hover: FlyHover, fetching: bool) -> Result<()> {
        let (r_refresh, r_gear) = fly_btns();
        let hover_bg = self.brush(pal.control_hover)?;
        if hover == FlyHover::Refresh {
            self.rounded(r_refresh, 4.0, &hover_bg)?;
        }
        if hover == FlyHover::Gear {
            self.rounded(r_gear, 4.0, &hover_bg)?;
        }
        let refresh_brush = self.brush(if fetching { pal.dim } else { pal.text })?;
        let gear_brush = self.brush(pal.text)?;
        self.glyph("\u{E72C}", r_refresh, &refresh_brush)?; // Refresh (dim = in flight)
        self.glyph("\u{E713}", r_gear, &gear_brush)?; // Settings
        Ok(())
    }

    fn glyph(&self, s: &str, r: D2D_RECT_F, brush: &ID2D1SolidColorBrush) -> Result<()> {
        unsafe {
            let f = self.fmt(w!("Segoe Fluent Icons"), 13.0, DWRITE_FONT_WEIGHT_NORMAL)?;
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            let wide: Vec<u16> = s.encode_utf16().collect();
            self.dc.DrawText(
                &wide,
                &f,
                &r,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            Ok(())
        }
    }

    fn draw_data(&self, pal: &Palette, w: f32, snap: &UsageSnapshot, note: Option<&str>) -> Result<()> {
        let text = self.brush(pal.text)?;
        let dim = self.brush(pal.dim)?;
        let track = self.brush(pal.track)?;
        let divider = self.brush(pal.divider)?;

        let title_fmt = self.fmt(w!("Segoe UI Variable Text"), SIZE_BODY, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let caption_fmt = self.fmt(w!("Segoe UI Variable Small"), SIZE_CAPTION, DWRITE_FONT_WEIGHT_NORMAL)?;
        self.text("Claude", &title_fmt, rect(PAD, PAD, w - PAD, PAD + TITLE_H), &text, false)?;

        let label_fmt = self.fmt(w!("Segoe UI Variable Text"), SIZE_BODY, DWRITE_FONT_WEIGHT_NORMAL)?;
        let pct_fmt = self.fmt(w!("Segoe UI Variable Text"), SIZE_BODY, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;

        let mut y = PAD + TITLE_H + SECTION_GAP;
        for (i, row) in snap.rows.iter().enumerate() {
            if i > 0 {
                y += ROW_GAP;
            }
            let fill_rgb = util::severity_rgb(&row.severity, row.percent, pal.accent_rgb);
            let fill = self.brush(col_rgb(fill_rgb, 1.0))?;

            self.text(&row.label, &label_fmt, rect(PAD, y, w - PAD - 56.0, y + LABEL_H), &text, false)?;
            let pct_str = format!("{:.0}%", row.percent);
            self.text(&pct_str, &pct_fmt, rect(w - PAD - 56.0, y, w - PAD, y + LABEL_H), &text, true)?;

            let bar_y = y + LABEL_H + GAP;
            let bar_w = w - 2.0 * PAD;
            self.rounded(rect(PAD, bar_y, PAD + bar_w, bar_y + BAR_H), BAR_H / 2.0, &track)?;
            let frac = (row.percent / 100.0).clamp(0.0, 1.0) as f32;
            if frac > 0.005 {
                let fw = (bar_w * frac).max(BAR_H);
                self.rounded(rect(PAD, bar_y, PAD + fw, bar_y + BAR_H), BAR_H / 2.0, &fill)?;
            }

            if !row.reset_text.is_empty() {
                let cap_y = bar_y + BAR_H + GAP;
                self.text(&row.reset_text, &caption_fmt, rect(PAD, cap_y, w - PAD, cap_y + CAPTION_H), &dim, false)?;
            }
            y += ROW_BLOCK;
        }

        let div_y = y + FOOTER_GAP_ABOVE;
        self.fill(rect(PAD, div_y, w - PAD, div_y + 1.0), &divider);
        let foot_y = div_y + 1.0 + FOOTER_GAP_BELOW;
        let mut footer = format!("Updated {}", relative_time(snap.fetched_unix));
        if let Some(n) = note {
            footer.push_str(&format!(" · {n}"));
        } else if !snap.plan.is_empty() {
            footer.push_str(&format!(" · {}", snap.plan));
        }
        self.text(&footer, &caption_fmt, rect(PAD, foot_y, w - PAD, foot_y + CAPTION_H), &dim, false)?;
        Ok(())
    }

    fn draw_message(&self, pal: &Palette, w: f32, head: &str, body: Option<&str>) -> Result<()> {
        let text = self.brush(pal.text)?;
        let dim = self.brush(pal.dim)?;
        let h_fmt = self.fmt(w!("Segoe UI Variable Text"), SIZE_BODY, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let b_fmt = self.fmt(w!("Segoe UI Variable Small"), SIZE_CAPTION, DWRITE_FONT_WEIGHT_NORMAL)?;
        self.text(head, &h_fmt, rect(PAD, PAD + 8.0, w - PAD, PAD + 28.0), &text, false)?;
        if let Some(b) = body {
            self.text(b, &b_fmt, rect(PAD, PAD + 36.0, w - PAD, 108.0), &dim, false)?;
        }
        Ok(())
    }

    // ---------- settings ----------

    pub fn render_settings(
        &mut self,
        w_px: u32,
        h_px: u32,
        dpi: f32,
        st: &SettingsView,
        dark: bool,
        accent: (u8, u8, u8),
    ) -> Result<()> {
        self.ensure_size(w_px.max(8), h_px.max(8), dpi)?;
        unsafe {
            self.dc.BeginDraw();
            self.dc.Clear(None); // Mica shows through

            let pal = Palette::new(dark, accent);
            let text = self.brush(pal.text)?;
            let dim = self.brush(pal.dim)?;
            let card_stroke = self.brush(pal.card_stroke)?;

            let body_fmt = self.fmt(w!("Segoe UI Variable Text"), SIZE_BODY, DWRITE_FONT_WEIGHT_NORMAL)?;
            let caption_fmt = self.fmt(w!("Segoe UI Variable Small"), SIZE_CAPTION, DWRITE_FONT_WEIGHT_NORMAL)?;

            let labels = [
                "Caps Lock light shows Claude status",
                "Start with Windows",
                "Auto-refresh",
                "Refresh usage now",
                "Quit Claudometer",
            ];
            let cards = settings_rects();
            for (i, card) in cards.iter().enumerate() {
                let bg = self.brush(if st.hover == i as i32 {
                    pal.card_hover
                } else {
                    pal.card_bg
                })?;
                self.rounded(*card, 4.0, &bg)?;
                let rr = D2D1_ROUNDED_RECT {
                    rect: rect(card.left + 0.5, card.top + 0.5, card.right - 0.5, card.bottom - 0.5),
                    radiusX: 3.5,
                    radiusY: 3.5,
                };
                self.dc.DrawRoundedRectangle(&rr, &card_stroke, 1.0, None);

                let label_right = if i == 2 { card.right - 200.0 } else { card.right - 120.0 };
                self.text_v(labels[i], &body_fmt, rect(card.left + 16.0, card.top, label_right, card.bottom), &text)?;

                let cy = (card.top + card.bottom) / 2.0;
                match i {
                    0 => self.toggle(card.right - 16.0, cy, st.caps_on, &pal)?,
                    1 => self.toggle(card.right - 16.0, cy, st.autostart, &pal)?,
                    2 => self.interval_row(card, st.poll_secs, &pal)?,
                    3 => self.button(card.right - 16.0, cy, "Refresh", &pal)?,
                    4 => self.button(card.right - 16.0, cy, "Quit", &pal)?,
                    _ => {}
                }
            }

            let foot_y = cards[N_CARDS - 1].bottom + 12.0;
            self.text(
                &format!("Claudometer {} · data from api.anthropic.com", env!("CARGO_PKG_VERSION")),
                &caption_fmt,
                rect(SET_PAD, foot_y, SET_W - SET_PAD, foot_y + CAPTION_H),
                &dim,
                false,
            )?;

            self.dc.EndDraw(None, None)?;
            self.swap.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    /// Segmented interval pills (SelectorBar-style), selected = accent
    fn interval_row(&self, card: &D2D_RECT_F, poll_secs: u32, pal: &Palette) -> Result<()> {
        unsafe {
            let pills = interval_pills(card);
            let f = self.fmt(w!("Segoe UI Variable Small"), SIZE_CAPTION, DWRITE_FONT_WEIGHT_NORMAL)?;
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            for (i, pill) in pills.iter().enumerate() {
                let (secs, label) = INTERVALS[i];
                let selected = secs == poll_secs;
                if selected {
                    let bg = self.brush(col_rgb(pal.accent_rgb, 1.0))?;
                    self.rounded(*pill, 12.0, &bg)?;
                } else {
                    let bg = self.brush(pal.control_fill)?;
                    self.rounded(*pill, 12.0, &bg)?;
                    let stroke = self.brush(pal.control_stroke)?;
                    let rr = D2D1_ROUNDED_RECT {
                        rect: rect(pill.left + 0.5, pill.top + 0.5, pill.right - 0.5, pill.bottom - 0.5),
                        radiusX: 11.5,
                        radiusY: 11.5,
                    };
                    self.dc.DrawRoundedRectangle(&rr, &stroke, 1.0, None);
                }
                let brush = if selected {
                    self.brush(col(1.0, 1.0, 1.0, 1.0))? // TextOnAccent
                } else {
                    self.brush(pal.text)?
                };
                let wide: Vec<u16> = label.encode_utf16().collect();
                self.dc.DrawText(
                    &wide,
                    &f,
                    pill,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            Ok(())
        }
    }

    /// Fluent ToggleSwitch, right-aligned at (right_edge, cy)
    fn toggle(&self, right_edge: f32, cy: f32, on: bool, pal: &Palette) -> Result<()> {
        unsafe {
            let w = 40.0;
            let h = 20.0;
            let r = rect(right_edge - w, cy - h / 2.0, right_edge, cy + h / 2.0);
            if on {
                let fill = self.brush(col_rgb(pal.accent_rgb, 1.0))?;
                self.rounded(r, h / 2.0, &fill)?;
                let knob = self.brush(col(1.0, 1.0, 1.0, 1.0))?;
                let e = D2D1_ELLIPSE {
                    point: D2D_POINT_2F { x: r.right - 10.0, y: cy },
                    radiusX: 7.0,
                    radiusY: 7.0,
                };
                self.dc.FillEllipse(&e, &knob);
            } else {
                let stroke = self.brush(pal.strong_stroke)?;
                let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: h / 2.0, radiusY: h / 2.0 };
                self.dc.DrawRoundedRectangle(&rr, &stroke, 1.0, None);
                let e = D2D1_ELLIPSE {
                    point: D2D_POINT_2F { x: r.left + 10.0, y: cy },
                    radiusX: 6.0,
                    radiusY: 6.0,
                };
                self.dc.FillEllipse(&e, &stroke);
            }
            Ok(())
        }
    }

    /// Fluent standard button, right-aligned at (right_edge, cy)
    fn button(&self, right_edge: f32, cy: f32, label: &str, pal: &Palette) -> Result<()> {
        unsafe {
            let w = 84.0;
            let h = 32.0;
            let r = rect(right_edge - w, cy - h / 2.0, right_edge, cy + h / 2.0);
            let bg = self.brush(pal.control_fill)?;
            self.rounded(r, 4.0, &bg)?;
            let stroke = self.brush(pal.control_stroke)?;
            let rr = D2D1_ROUNDED_RECT {
                rect: rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5),
                radiusX: 3.5,
                radiusY: 3.5,
            };
            self.dc.DrawRoundedRectangle(&rr, &stroke, 1.0, None);
            let f = self.fmt(w!("Segoe UI Variable Text"), SIZE_BODY, DWRITE_FONT_WEIGHT_NORMAL)?;
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            let brush = self.brush(pal.text)?;
            let wide: Vec<u16> = label.encode_utf16().collect();
            self.dc.DrawText(
                &wide,
                &f,
                &r,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            Ok(())
        }
    }

    // ---------- primitives ----------

    fn brush(&self, c: D2D1_COLOR_F) -> Result<ID2D1SolidColorBrush> {
        unsafe {
            let rt: ID2D1RenderTarget = self.dc.cast()?;
            rt.CreateSolidColorBrush(&c, None)
        }
    }

    fn fmt(&self, family: PCWSTR, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextFormat> {
        unsafe {
            self.dwrite.CreateTextFormat(
                family,
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )
        }
    }

    fn text(
        &self,
        s: &str,
        f: &IDWriteTextFormat,
        r: D2D_RECT_F,
        brush: &ID2D1SolidColorBrush,
        trailing: bool,
    ) -> Result<()> {
        unsafe {
            f.SetTextAlignment(if trailing {
                DWRITE_TEXT_ALIGNMENT_TRAILING
            } else {
                DWRITE_TEXT_ALIGNMENT_LEADING
            })?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
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

    /// vertically-centered text
    fn text_v(
        &self,
        s: &str,
        f: &IDWriteTextFormat,
        r: D2D_RECT_F,
        brush: &ID2D1SolidColorBrush,
    ) -> Result<()> {
        unsafe {
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
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
            let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
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
    accent_rgb: (u8, u8, u8),
}

impl Palette {
    fn new(dark: bool, accent: (u8, u8, u8)) -> Self {
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
                accent_rgb: accent,
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
                accent_rgb: accent,
            }
        }
    }
}

fn col(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn col_rgb(rgb: (u8, u8, u8), a: f32) -> D2D1_COLOR_F {
    col(rgb.0 as f32 / 255.0, rgb.1 as f32 / 255.0, rgb.2 as f32 / 255.0, a)
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
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
