//! Theme, accent color, autostart, dark context menus.

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::*;
use windows::UI::ViewManagement::{UIColorType, UISettings};

const PERSONALIZE: PCWSTR =
    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("Claudometer");

pub fn is_dark_theme() -> bool {
    unsafe {
        let mut val: u32 = 1;
        let mut size = std::mem::size_of::<u32>() as u32;
        let ok = RegGetValueW(
            HKEY_CURRENT_USER,
            PERSONALIZE,
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut val as *mut u32 as *mut _),
            Some(&mut size),
        );
        ok == ERROR_SUCCESS && val == 0
    }
}

pub fn accent_rgb() -> (u8, u8, u8) {
    (|| -> Result<(u8, u8, u8)> {
        let ui = UISettings::new()?;
        let c = ui.GetColorValue(UIColorType::Accent)?;
        Ok((c.R, c.G, c.B))
    })()
    .unwrap_or((0, 120, 212)) // Windows default blue
}

/// Ring / bar fill color by severity, with percent fallback thresholds.
pub fn severity_rgb(severity: &str, percent: f64, accent: (u8, u8, u8)) -> (u8, u8, u8) {
    let s = severity.to_ascii_lowercase();
    if s.contains("exceed") || s.contains("critical") || s.contains("error") || percent >= 100.0 {
        (232, 17, 35) // Fluent red
    } else if s.contains("warn") || s.contains("elevated") || percent >= 85.0 {
        (255, 185, 0) // Fluent amber
    } else {
        accent
    }
}

pub fn autostart_enabled() -> bool {
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            RUN_VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        ) == ERROR_SUCCESS
    }
}

pub fn set_autostart(on: bool) {
    unsafe {
        if on {
            let Ok(exe) = std::env::current_exe() else { return };
            let cmd = format!("\"{}\"", exe.display());
            let wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(wide.as_ptr() as *const _),
                (wide.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
}

// ---------- app config (%APPDATA%\Claudometer\settings.json) ----------

pub fn config_dir() -> Option<std::path::PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(std::path::Path::new(&appdata).join("Claudometer"))
}

fn config_path() -> Option<std::path::PathBuf> {
    Some(config_dir()?.join("settings.json"))
}

fn read_config() -> serde_json::Value {
    config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

fn write_config_field(key: &str, value: serde_json::Value) {
    let Some(p) = config_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut cfg = read_config();
    cfg[key] = value;
    if let Ok(s) = serde_json::to_string_pretty(&cfg) {
        let _ = std::fs::write(p, s + "\n");
    }
}

pub fn load_poll_secs() -> u32 {
    read_config()
        .get("poll_secs")
        .and_then(|x| x.as_u64())
        .map(|x| (x as u32).clamp(30, 300))
        .unwrap_or(60)
}

pub fn save_poll_secs(secs: u32) {
    write_config_field("poll_secs", secs.into());
}

/// Codex section toggle — defaults on; section still only shows when a
/// Codex sign-in actually exists on disk.
pub fn show_codex() -> bool {
    read_config()
        .get("show_codex")
        .and_then(|x| x.as_bool())
        .unwrap_or(true)
}

pub fn set_show_codex(on: bool) {
    write_config_field("show_codex", on.into());
}

/// Toast alerts when a limit window crosses the warn threshold — default on.
pub fn alerts_enabled() -> bool {
    read_config()
        .get("alerts")
        .and_then(|x| x.as_bool())
        .unwrap_or(true)
}

pub fn set_alerts_enabled(on: bool) {
    write_config_field("alerts", on.into());
}

/// Alert dedup, persisted so a restart mid-window doesn't re-alert:
/// map of limit key → `resets_at` epoch that already fired.
pub fn load_alerted() -> std::collections::HashMap<String, i64> {
    read_config()
        .get("alerted")
        .and_then(|v| v.as_object().cloned())
        .map(|o| {
            o.into_iter()
                .filter_map(|(k, v)| Some((k, v.as_i64()?)))
                .collect()
        })
        .unwrap_or_default()
}

pub fn save_alerted(map: &std::collections::HashMap<String, i64>) {
    write_config_field("alerted", serde_json::json!(map));
}

// ---------- Caps-LED status hook toggle ----------

fn caps_hooks_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var("USERPROFILE").ok()?;
    Some(std::path::Path::new(&home).join(".claude").join("hooks"))
}

pub fn caps_led_enabled() -> bool {
    caps_hooks_dir()
        .map(|d| !d.join("caps-led.disabled").exists())
        .unwrap_or(false)
}

pub fn set_caps_led_enabled(on: bool) {
    let Some(dir) = caps_hooks_dir() else { return };
    let marker = dir.join("caps-led.disabled");
    if on {
        let _ = std::fs::remove_file(marker);
    } else {
        // stop any running flasher + LED off, then drop the marker
        run_caps_script(&dir, "end");
        let _ = std::fs::write(marker, "disabled via Claudometer settings\n");
    }
}

fn run_caps_script(dir: &std::path::Path, mode: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = dir.join("caps-led.ps1");
    if !script.exists() {
        return;
    }
    let _ = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .arg(mode)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// Undocumented accent-policy acrylic: blurs whatever is behind the window and
/// tints it. Works on borderless popups where DWMWA_SYSTEMBACKDROP_TYPE only
/// renders its opaque fallback. ACCENT_ENABLE_ACRYLICBLURBEHIND = 4,
/// WCA_ACCENT_POLICY = 19, tint is AABBGGRR.
pub fn apply_acrylic(hwnd: HWND, dark: bool) {
    #[repr(C)]
    struct AccentPolicy {
        state: i32,
        flags: i32,
        gradient: u32,
        anim: i32,
    }
    #[repr(C)]
    struct CompAttrData {
        attrib: i32,
        pv: *mut core::ffi::c_void,
        size: usize,
    }
    unsafe {
        let Ok(user32) = LoadLibraryW(w!("user32.dll")) else { return };
        let Some(f) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else {
            return;
        };
        let set_wca: extern "system" fn(HWND, *mut CompAttrData) -> BOOL =
            std::mem::transmute(f);
        let tint: u32 = if dark { 0xCC_20_20_20 } else { 0xCC_F3_F3_F3 };
        let mut policy = AccentPolicy {
            state: 4, // ACCENT_ENABLE_ACRYLICBLURBEHIND
            flags: 2,
            gradient: tint,
            anim: 0,
        };
        let mut data = CompAttrData {
            attrib: 19, // WCA_ACCENT_POLICY
            pv: &mut policy as *mut _ as *mut _,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        let _ = set_wca(hwnd, &mut data);
    }
}

/// Undocumented uxtheme ordinals — makes Win32 popup menus follow dark mode.
/// Ordinal 135 = SetPreferredAppMode(AllowDark), 136 = FlushMenuThemes.
pub fn enable_dark_context_menus() {
    unsafe {
        let Ok(lib) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(p135) = GetProcAddress(lib, PCSTR(135usize as *const u8)) {
            let set_preferred_app_mode: extern "system" fn(i32) -> i32 =
                std::mem::transmute(p135);
            set_preferred_app_mode(1); // AllowDark
        }
        if let Some(p136) = GetProcAddress(lib, PCSTR(136usize as *const u8)) {
            let flush_menu_themes: extern "system" fn() = std::mem::transmute(p136);
            flush_menu_themes();
        }
    }
}
