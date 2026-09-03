//! Theme, accent color, autostart, dark context menus.

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::*;
use windows::UI::ViewManagement::{UIColorType, UISettings};

const PERSONALIZE: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
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
            let Ok(exe) = std::env::current_exe() else {
                return;
            };
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
    crate::config::config_dir()
}

// ---------- Caps-LED status hook toggle ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapsLedState {
    Unavailable,
    InstalledDisabled,
    InstalledEnabled,
    Error(CapsLedError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapsLedError {
    ScriptRead,
    SettingsRead,
    SettingsMalformed,
    MarkerRead,
    MarkerWrite,
    MarkerRemove,
    ScriptLaunch,
}

impl CapsLedError {
    pub fn message(self) -> &'static str {
        match self {
            Self::ScriptRead => "script check failed",
            Self::SettingsRead => "settings read failed",
            Self::SettingsMalformed => "settings malformed",
            Self::MarkerRead => "marker read failed",
            Self::MarkerWrite => "marker write failed",
            Self::MarkerRemove => "marker removal failed",
            Self::ScriptLaunch => "script launch failed",
        }
    }
}

#[derive(Clone, Copy)]
struct CapsLedFailure {
    error: CapsLedError,
    desired_enabled: bool,
}

static CAPS_LED_FAILURE: std::sync::Mutex<Option<CapsLedFailure>> = std::sync::Mutex::new(None);

fn claude_config_dir() -> Option<std::path::PathBuf> {
    claude_config_dir_from(
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        std::env::var_os("USERPROFILE"),
    )
}

fn claude_config_dir_from(
    configured: Option<std::ffi::OsString>,
    user_profile: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    configured
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            user_profile
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".claude"))
        })
}

pub fn caps_led_state() -> CapsLedState {
    if let Some(failure) = *CAPS_LED_FAILURE.lock().unwrap() {
        return CapsLedState::Error(failure.error);
    }
    claude_config_dir()
        .as_deref()
        .map(inspect_caps_led_at)
        .unwrap_or(CapsLedState::Unavailable)
}

fn inspect_caps_led_at(config_dir: &std::path::Path) -> CapsLedState {
    let hooks_dir = config_dir.join("hooks");
    match std::fs::metadata(hooks_dir.join("caps-led.ps1")) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return CapsLedState::Unavailable,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CapsLedState::Unavailable;
        }
        Err(_) => return CapsLedState::Error(CapsLedError::ScriptRead),
    }

    let settings = match std::fs::read(config_dir.join("settings.json")) {
        Ok(settings) => settings,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CapsLedState::Unavailable;
        }
        Err(_) => return CapsLedState::Error(CapsLedError::SettingsRead),
    };
    let settings: serde_json::Value = match serde_json::from_slice(&settings) {
        Ok(settings) => settings,
        Err(_) => return CapsLedState::Error(CapsLedError::SettingsMalformed),
    };
    if !caps_hook_configured(&settings) {
        return CapsLedState::Unavailable;
    }

    match std::fs::metadata(hooks_dir.join("caps-led.disabled")) {
        Ok(metadata) if metadata.is_file() => CapsLedState::InstalledDisabled,
        Ok(_) => CapsLedState::Error(CapsLedError::MarkerRead),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            CapsLedState::InstalledEnabled
        }
        Err(_) => CapsLedState::Error(CapsLedError::MarkerRead),
    }
}

fn caps_hook_configured(settings: &serde_json::Value) -> bool {
    const EVENTS: [&str; 4] = ["UserPromptSubmit", "Stop", "Notification", "SessionEnd"];
    let Some(hooks) = settings.get("hooks") else {
        return false;
    };
    EVENTS.iter().any(|event| {
        hooks
            .get(*event)
            .is_some_and(command_references_caps_script)
    })
}

fn command_references_caps_script(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(values) => values.iter().any(command_references_caps_script),
        serde_json::Value::Object(values) => values.iter().any(|(key, value)| {
            if key == "command" {
                value
                    .as_str()
                    .is_some_and(|command| command.to_ascii_lowercase().contains("caps-led.ps1"))
            } else {
                command_references_caps_script(value)
            }
        }),
        _ => false,
    }
}

pub fn set_caps_led_enabled(on: bool) -> std::result::Result<(), CapsLedError> {
    let Some(config_dir) = claude_config_dir() else {
        *CAPS_LED_FAILURE.lock().unwrap() = None;
        return Ok(());
    };
    match inspect_caps_led_at(&config_dir) {
        CapsLedState::Unavailable => {
            *CAPS_LED_FAILURE.lock().unwrap() = None;
            return Ok(());
        }
        CapsLedState::Error(error) => {
            *CAPS_LED_FAILURE.lock().unwrap() = Some(CapsLedFailure {
                error,
                desired_enabled: on,
            });
            return Err(error);
        }
        CapsLedState::InstalledDisabled | CapsLedState::InstalledEnabled => {}
    }
    let hooks_dir = config_dir.join("hooks");
    let result = update_caps_led_at(
        &hooks_dir,
        on,
        |path| std::fs::write(path, "disabled via Claudometer settings\n"),
        |path| std::fs::remove_file(path),
        run_caps_script,
    );
    *CAPS_LED_FAILURE.lock().unwrap() = result.err().map(|error| CapsLedFailure {
        error,
        desired_enabled: on,
    });
    result
}

pub fn retry_caps_led() -> std::result::Result<(), CapsLedError> {
    let desired_enabled = CAPS_LED_FAILURE
        .lock()
        .unwrap()
        .map(|failure| failure.desired_enabled);
    match desired_enabled {
        Some(desired_enabled) => set_caps_led_enabled(desired_enabled),
        None => Ok(()),
    }
}

fn update_caps_led_at(
    dir: &std::path::Path,
    on: bool,
    write_marker: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    remove_marker: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    launch_script: impl FnOnce(&std::path::Path, &str) -> std::io::Result<()>,
) -> std::result::Result<(), CapsLedError> {
    let marker = dir.join("caps-led.disabled");
    if on {
        match remove_marker(&marker) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(CapsLedError::MarkerRemove),
        }
    } else {
        // Block new hook work first, then stop any flasher and turn the LED off.
        write_marker(&marker).map_err(|_| CapsLedError::MarkerWrite)?;
        launch_script(dir, "end").map_err(|_| CapsLedError::ScriptLaunch)
    }
}

fn run_caps_script(dir: &std::path::Path, mode: &str) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = dir.join("caps-led.ps1");
    std::process::Command::new("powershell")
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
        .spawn()
        .map(|_| ())
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
        let Ok(user32) = LoadLibraryW(w!("user32.dll")) else {
            return;
        };
        let Some(f) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else {
            return;
        };
        let set_wca: extern "system" fn(HWND, *mut CompAttrData) -> BOOL = std::mem::transmute(f);
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
        let Ok(lib) = LoadLibraryW(w!("uxtheme.dll")) else {
            return;
        };
        if let Some(p135) = GetProcAddress(lib, PCSTR(135usize as *const u8)) {
            let set_preferred_app_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(p135);
            set_preferred_app_mode(1); // AllowDark
        }
        if let Some(p136) = GetProcAddress(lib, PCSTR(136usize as *const u8)) {
            let flush_menu_themes: extern "system" fn() = std::mem::transmute(p136);
            flush_menu_themes();
        }
    }
}

#[cfg(test)]
mod caps_led_tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let unique = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "claudometer-caps-test-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn install(&self) {
            let hooks = self.0.join("hooks");
            std::fs::create_dir(&hooks).unwrap();
            std::fs::write(hooks.join("caps-led.ps1"), "# sanitized fixture\n").unwrap();
            std::fs::write(
                self.0.join("settings.json"),
                r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"powershell -File hooks/caps-led.ps1 done"}]}]}}"#,
            )
            .unwrap();
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let temp = std::env::temp_dir();
            assert!(self.0.starts_with(&temp));
            assert!(self
                .0
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("claudometer-caps-test-")));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn custom_claude_config_directory_wins_over_user_profile() {
        assert_eq!(
            claude_config_dir_from(Some("D:\\ClaudeProfile".into()), Some("C:\\User".into())),
            Some(std::path::PathBuf::from("D:\\ClaudeProfile"))
        );
        assert_eq!(
            claude_config_dir_from(None, Some("C:\\User".into())),
            Some(std::path::PathBuf::from("C:\\User\\.claude"))
        );
    }

    #[test]
    fn clean_and_partial_installations_are_unavailable() {
        let directory = TestDirectory::new();
        assert_eq!(inspect_caps_led_at(&directory.0), CapsLedState::Unavailable);

        let hooks = directory.0.join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        std::fs::write(hooks.join("caps-led.ps1"), "# sanitized fixture\n").unwrap();
        assert_eq!(inspect_caps_led_at(&directory.0), CapsLedState::Unavailable);

        std::fs::write(directory.0.join("settings.json"), r#"{"hooks":{}}"#).unwrap();
        assert_eq!(inspect_caps_led_at(&directory.0), CapsLedState::Unavailable);
    }

    #[test]
    fn complete_installation_reports_enabled_and_disabled_states() {
        let directory = TestDirectory::new();
        directory.install();
        assert_eq!(
            inspect_caps_led_at(&directory.0),
            CapsLedState::InstalledEnabled
        );

        std::fs::write(
            directory.0.join("hooks").join("caps-led.disabled"),
            "disabled\n",
        )
        .unwrap();
        assert_eq!(
            inspect_caps_led_at(&directory.0),
            CapsLedState::InstalledDisabled
        );
    }

    #[test]
    fn malformed_hook_settings_are_an_error() {
        let directory = TestDirectory::new();
        let hooks = directory.0.join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        std::fs::write(hooks.join("caps-led.ps1"), "# sanitized fixture\n").unwrap();
        std::fs::write(directory.0.join("settings.json"), "{").unwrap();
        assert_eq!(
            inspect_caps_led_at(&directory.0),
            CapsLedState::Error(CapsLedError::SettingsMalformed)
        );
    }

    #[test]
    fn write_remove_and_launch_failures_are_returned() {
        let path = std::path::Path::new("ignored");
        let denied = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            update_caps_led_at(path, false, |_| Err(denied()), |_| Ok(()), |_, _| Ok(())),
            Err(CapsLedError::MarkerWrite)
        );
        assert_eq!(
            update_caps_led_at(path, false, |_| Ok(()), |_| Ok(()), |_, _| Err(denied())),
            Err(CapsLedError::ScriptLaunch)
        );
        assert_eq!(
            update_caps_led_at(path, true, |_| Ok(()), |_| Err(denied()), |_, _| Ok(())),
            Err(CapsLedError::MarkerRemove)
        );
    }
}
