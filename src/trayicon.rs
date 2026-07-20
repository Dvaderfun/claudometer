//! CPU-rasterized tray icon: anti-aliased progress ring → HICON.
//! No font dependency; premultiplied BGRA DIB + CreateIconIndirect.

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub enum Style {
    /// Progress ring: frac 0..=1, fill color
    Ring { frac: f32, rgb: (u8, u8, u8) },
    /// Dim full ring (no data yet)
    Loading,
    /// Dim ring + amber "!"
    Alert,
}

pub fn build(style: &Style, dark: bool) -> Option<HICON> {
    unsafe {
        let s = GetSystemMetrics(SM_CXSMICON).max(16);
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = s;
        bmi.bmiHeader.biHeight = -s; // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let hbm = CreateDIBSection(
            HDC::default(),
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
        .ok()?;
        let px = bits as *mut u32;

        rasterize(px, s as usize, style, dark);

        let hmask = CreateBitmap(s, s, 1, 1, None);
        let ii = ICONINFO {
            fIcon: TRUE,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hmask,
            hbmColor: hbm,
        };
        let icon = CreateIconIndirect(&ii).ok();
        let _ = DeleteObject(hbm);
        let _ = DeleteObject(hmask);
        icon
    }
}

fn rasterize(px: *mut u32, s: usize, style: &Style, dark: bool) {
    let sf = s as f32;
    let c = (sf - 1.0) / 2.0;
    let thickness = (sf * 0.19).max(2.0);
    let r_mid = sf * 0.5 - thickness / 2.0 - 0.7;
    const TAU: f32 = std::f32::consts::TAU;

    let (track_rgb, track_a) = if dark {
        ((255u8, 255u8, 255u8), 0.28f32)
    } else {
        ((0u8, 0u8, 0u8), 0.22f32)
    };

    let (frac, fill_rgb): (f32, (u8, u8, u8)) = match style {
        Style::Ring { frac, rgb } => (frac.clamp(0.0, 1.0), *rgb),
        Style::Loading => (0.0, (0, 0, 0)),
        Style::Alert => (0.0, (0, 0, 0)),
    };

    for y in 0..s {
        for x in 0..s {
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let d = (dx * dx + dy * dy).sqrt();
            // ring band coverage with ~0.7px AA
            let band = (thickness / 2.0 + 0.7 - (d - r_mid).abs()).clamp(0.0, 1.0);
            let mut out = 0u32;
            if band > 0.0 {
                // angle 0 at 12 o'clock, clockwise, normalized 0..1
                let mut a = dx.atan2(-dy);
                if a < 0.0 {
                    a += TAU;
                }
                let a = a / TAU;
                // angular AA width ≈ one pixel of arc length
                let w = 1.0 / (TAU * d.max(1.0));
                let m = if frac >= 1.0 {
                    1.0
                } else {
                    ((frac - a) / w + 0.5).clamp(0.0, 1.0)
                };
                let r = lerp(track_rgb.0, fill_rgb.0, m);
                let g = lerp(track_rgb.1, fill_rgb.1, m);
                let b = lerp(track_rgb.2, fill_rgb.2, m);
                let alpha = band * (track_a + (1.0 - track_a) * m);
                out = premul(r, g, b, alpha);
            }
            unsafe { *px.add(y * s + x) = out };
        }
    }

    if matches!(style, Style::Alert) {
        // amber "!": bar + dot, centered
        let (ar, ag, ab) = (255u8, 185u8, 0u8);
        let half_w = (sf * 0.07).max(1.0);
        let bar_top = sf * 0.24;
        let bar_bot = sf * 0.56;
        let dot_top = sf * 0.66;
        let dot_bot = sf * 0.66 + half_w * 2.0;
        for y in 0..s {
            for x in 0..s {
                let fx = x as f32;
                let fy = y as f32;
                let in_x = (half_w - (fx - c).abs() + 0.5).clamp(0.0, 1.0);
                if in_x <= 0.0 {
                    continue;
                }
                let in_bar = (cov(fy, bar_top, bar_bot)).max(cov(fy, dot_top, dot_bot));
                let alpha = in_x * in_bar;
                if alpha > 0.0 {
                    unsafe { *px.add(y * s + x) = premul(ar, ag, ab, alpha) };
                }
            }
        }
    }
}

fn cov(v: f32, lo: f32, hi: f32) -> f32 {
    // 1 inside [lo, hi] with 0.5px soft edges
    ((v - lo + 0.5).clamp(0.0, 1.0)) * ((hi - v + 0.5).clamp(0.0, 1.0))
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

fn premul(r: u8, g: u8, b: u8, a: f32) -> u32 {
    let a = a.clamp(0.0, 1.0);
    let pa = (a * 255.0).round() as u32;
    let pr = (r as f32 * a).round() as u32;
    let pg = (g as f32 * a).round() as u32;
    let pb = (b as f32 * a).round() as u32;
    (pa << 24) | (pr << 16) | (pg << 8) | pb
}
