//! Claudometer — Claude usage limits in the Windows 11 tray.
//! Native Win32: tray ring icon + acrylic DirectComposition flyout + Mica settings.

#![windows_subsystem = "windows"]
#![allow(clippy::missing_safety_doc)]

mod accessibility;
mod alerts;
mod api;
mod app;
mod auth;
mod codex;
#[cfg(target_arch = "x86_64")]
mod codex_server;
#[cfg(not(target_arch = "x86_64"))]
mod codex_server {
    pub struct Executable;
    pub fn discover() -> Result<Executable, &'static str> {
        Err("app-server version not audited")
    }
    pub fn fetch(
        _: &Executable,
        _: &crate::codex::PreparedRequest,
    ) -> Result<crate::provider::model::UsageSnapshot, crate::provider::error::FetchError> {
        Err(crate::provider::error::FetchError::new(
            crate::provider::error::FailureKind::Transient,
        ))
    }
    pub fn measure() -> windows::core::Result<()> {
        crate::diagnostics::write_stdout(b"{\"error\":\"app_server_unavailable\"}\n")
    }
}
mod config;
mod demo;
mod diagnostics;
mod gfx;
mod network;
mod poller;
pub mod provider;
mod release_manifest;
pub mod runtime_state;
#[cfg(test)]
mod state_reference;
pub mod store;
mod trayicon;
mod updater;
mod util;
mod vibecode;

use std::cell::RefCell;
use std::sync::atomic::{AtomicI32, AtomicIsize, AtomicU32, Ordering};
use std::time::Duration;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
};
use windows::Win32::UI::Controls::{
    MARGINS, TOOLTIPS_CLASSW, TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW, TTM_SETMAXTIPWIDTH,
    TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_END, VK_ESCAPE, VK_HOME,
    VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use provider::model::{LimitKind, ProviderId, UsageSnapshot};
use provider::state::{Clock, RefreshTrigger, SystemClock};

const WM_TRAY: u32 = WM_APP + 1;
const WM_DATA_READY: u32 = WM_APP + 2;
/// toast clicked (posted from a WinRT threadpool thread by alerts.rs)
pub(crate) const WM_TOAST_ACTIVATED: u32 = WM_APP + 3;
/// updater state changed (wparam 0) / handover ready, quit now (wparam 1)
pub(crate) const WM_UPDATE: u32 = WM_APP + 4;
/// Claude Code auth status/login completed on its worker thread.
const WM_AUTH_READY: u32 = WM_APP + 5;

const IDM_REFRESH: usize = 1;
const IDM_AUTOSTART: usize = 2;
const IDM_QUIT: usize = 3;
const IDM_SETTINGS: usize = 4;
const TIMER_POLL: usize = 1;
const TIMER_TICK: usize = 3; // 30s repaint so the relative "Updated…" label never freezes
const TRAY_ID: u32 = 1;

// NOTIFYICON_VERSION_4 tray events (not exported by windows 0.58)
const EVT_NIN_SELECT: u32 = 0x400;
const EVT_NIN_KEYSELECT: u32 = 0x401;
// not exported by windows 0.58 either
const MSG_MOUSELEAVE: u32 = 0x02A3;

pub(crate) static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static FLYOUT_HWND: AtomicIsize = AtomicIsize::new(0);
static SETTINGS_HWND: AtomicIsize = AtomicIsize::new(0);
static PREV_ICON: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_MSG: AtomicU32 = AtomicU32::new(0);
static ANCHOR_X: AtomicI32 = AtomicI32::new(0);
static ANCHOR_Y: AtomicI32 = AtomicI32::new(0);
static POLL_SECS: AtomicU32 = AtomicU32::new(60);

fn any_fetching() -> bool {
    app::any_fetching()
}

/// Codex section is live: toggle on AND a ChatGPT-login auth file on disk.
fn codex_active() -> bool {
    config::settings().codex_enabled && codex::available()
}

fn effective(provider: ProviderId) -> (Option<UsageSnapshot>, Option<String>) {
    app::effective(provider)
}
struct Ui {
    error_tooltip: Option<HWND>,
    error_tooltip_text: Vec<u16>,
    diagnostics_copy: &'static str,
    fly: Option<gfx::Surface>,
    set: Option<gfx::Surface>,
    fly_hover: gfx::FlyHover,
    set_hover: i32,
    fly_focus: i32, // keyboard focus: -1 none, 0 refresh, 1 gear, 2 vibecode
    fly_scroll: f32,
    set_focus: i32, // keyboard focus card index, -1 none
    set_scroll: f32,
    fly_tracking: bool,
    set_tracking: bool,
    /// top of the Vibecode row in flyout DIP coords — cached at render time so
    /// hit-testing on every mouse move doesn't rebuild the whole view
    fly_vibe_top: f32,
}

/// Flyout keyboard focus targets (refresh, gear, Vibecode row).
const FLY_FOCUS_N: i32 = 3;

thread_local! {
    static UI: RefCell<Ui> = const {
        RefCell::new(Ui {
            error_tooltip: None,
            error_tooltip_text: Vec::new(),
            diagnostics_copy: "Copy",
            fly: None,
            set: None,
            fly_hover: gfx::FlyHover::None,
            set_hover: -1,
            fly_focus: -1,
            fly_scroll: 0.0,
            set_focus: -1,
            set_scroll: 0.0,
            fly_tracking: false,
            set_tracking: false,
            fly_vibe_top: 0.0,
        })
    };
}

#[inline(never)]
unsafe fn create_window(
    exstyle: WINDOW_EX_STYLE,
    class: PCWSTR,
    title: PCWSTR,
    style: WINDOW_STYLE,
    bounds: [i32; 4],
    parent: HWND,
    instance: HINSTANCE,
) -> Result<HWND> {
    CreateWindowExW(
        exstyle, class, title, style, bounds[0], bounds[1], bounds[2], bounds[3], parent, None,
        instance, None,
    )
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(result) = diagnostics::support_command(&args) {
        return result;
    }
    if args
        .iter()
        .any(|argument| argument == "--measure-codex-source")
    {
        if args.iter().any(|argument| argument.starts_with("--demo")) {
            return diagnostics::write_stdout(b"{\"error\":\"demo_no_live_measurement\"}\n");
        }
        return codex_server::measure();
    }
    if let Some(result) = updater::run_watchdog_if_requested(&args) {
        return result.map_err(|message| Error::new(E_FAIL, message));
    }
    let demo_request =
        demo::parse_request(&args).map_err(|message| Error::new(E_INVALIDARG, message))?;
    if let Some(request) = demo_request {
        demo::activate(request, SystemClock.read().unix_seconds)
            .map_err(|message| Error::new(E_UNEXPECTED, message))?;
    }

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);

        if !demo::is_active() && args.iter().any(|argument| argument == "--recover-vibecode") {
            config::initialize();
            return if vibecode::recover_command() {
                Ok(())
            } else {
                Err(Error::new(E_FAIL, "Vibecode recovery remains unresolved"))
            };
        }

        // hidden verification hook: fire a fake 75% toast and exit
        if !demo::is_active() && args.iter().any(|argument| argument == "--test-alert") {
            alerts::init();
            alerts::show_test();
            std::thread::sleep(std::time::Duration::from_secs(2));
            return Ok(());
        }

        let _instance_mutex = if demo::is_active() {
            None
        } else {
            let mutex = CreateMutexW(None, true, w!("Local\\Claudometer.SingleInstance"))?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                // normal double-launch: exit silently. `--swap-wait` = we're the
                // fresh exe of an update handover — the old instance is exiting;
                // its mutex signals abandoned once its process dies.
                if !args.iter().any(|argument| argument == "--swap-wait") {
                    return Ok(());
                }
                let wait = WaitForSingleObject(mutex, 15_000);
                if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
                    return Ok(());
                }
            }
            Some(mutex)
        };

        let update_startup = if demo::is_active() {
            None
        } else {
            Some(updater::prepare_startup(&args).map_err(|message| Error::new(E_FAIL, message))?)
        };

        if !demo::is_active() {
            if update_startup
                .as_ref()
                .is_some_and(updater::StartupGuard::compatibility_mode)
            {
                config::initialize_compatibility();
            } else {
                config::initialize();
            }
            runtime_state::initialize();
            diagnostics::initialize_log();
            util::enable_dark_context_menus();
            alerts::init();
        }

        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        TASKBAR_MSG.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::SeqCst,
        );

        // hidden main window (tray owner + broadcast receiver)
        let cls = w!("Claudometer.Main");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(main_wndproc),
            hInstance: hinst,
            lpszClassName: cls,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let main = create_window(
            WINDOW_EX_STYLE(0),
            cls,
            w!("Claudometer"),
            WS_POPUP,
            [0, 0, 0, 0],
            HWND::default(),
            hinst,
        )?;
        MAIN_HWND.store(main.0 as isize, Ordering::SeqCst);

        let arrow_cursor = LoadCursorW(None, IDC_ARROW)?;
        // flyout window
        let fcls = w!("Claudometer.Flyout");
        let fwc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(flyout_wndproc),
            hInstance: hinst,
            hCursor: arrow_cursor,
            lpszClassName: fcls,
            ..Default::default()
        };
        RegisterClassExW(&fwc);
        let flyout = create_window(
            WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST,
            fcls,
            w!("Claude usage"),
            WS_POPUP,
            [0, 0, 10, 10],
            HWND::default(),
            hinst,
        )?;
        FLYOUT_HWND.store(flyout.0 as isize, Ordering::SeqCst);
        style_flyout(flyout);

        // settings window class (window created lazily); icon = embedded resource id 1
        // MAKEINTRESOURCE(1): the resource id is smuggled through the pointer
        // value — clippy's `dangling::<u16>()` would be address 2, wrong id.
        #[allow(clippy::manual_dangling_ptr)]
        let app_icon = LoadIconW(hinst, PCWSTR(1usize as *const u16)).unwrap_or_default();
        let scls = w!("Claudometer.Settings");
        let swc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(settings_wndproc),
            hInstance: hinst,
            hCursor: arrow_cursor,
            hIcon: app_icon,
            lpszClassName: scls,
            ..Default::default()
        };
        RegisterClassExW(&swc);

        add_tray_icon(main);
        if let Some(state) = demo::active() {
            signal_demo_ready(state);
            if !state.hidden {
                if state.scenario == demo::Scenario::Settings {
                    open_settings();
                } else {
                    show_demo_flyout(flyout, state);
                }
            }
        } else {
            updater::complete_startup(update_startup.expect("non-demo startup guard"))
                .map_err(|message| Error::new(E_FAIL, message))?;
            config::commit_pending_migration();
            // wake lock is per-thread — must be armed (and dropped) on this thread
            vibecode::init();
            POLL_SECS.store(config::settings().poll_interval_seconds, Ordering::SeqCst);
            SetTimer(
                main,
                TIMER_POLL,
                POLL_SECS.load(Ordering::SeqCst) * 1000,
                None,
            );
            // TIMER_TICK runs only while the flyout is visible (started in show_flyout)
            spawn_claude_account(false);
            spawn_fetch_all(RefreshTrigger::Automatic);
            updater::maybe_check();
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn flyout_hwnd() -> HWND {
    HWND(FLYOUT_HWND.load(Ordering::SeqCst) as *mut _)
}

fn settings_hwnd() -> HWND {
    HWND(SETTINGS_HWND.load(Ordering::SeqCst) as *mut _)
}

// ---------- window procs ----------

extern "system" fn main_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAY => {
                let event = (lparam.0 as u32) & 0xFFFF;
                let x = (wparam.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                match event {
                    e if e == EVT_NIN_SELECT || e == EVT_NIN_KEYSELECT => toggle_flyout(x, y),
                    e if e == WM_CONTEXTMENU => show_menu(hwnd, x, y),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DATA_READY => {
                if !app::drain_events(Duration::from_secs(u64::from(
                    POLL_SECS.load(Ordering::SeqCst),
                ))) {
                    return LRESULT(0);
                }
                update_tray(hwnd);
                if IsWindowVisible(flyout_hwnd()).as_bool() {
                    show_flyout(
                        ANCHOR_X.load(Ordering::SeqCst),
                        ANCHOR_Y.load(Ordering::SeqCst),
                    );
                }
                let sh = settings_hwnd();
                if !sh.is_invalid() && IsWindowVisible(sh).as_bool() {
                    render_settings(sh);
                }
                LRESULT(0)
            }
            WM_AUTH_READY => {
                if wparam.0 == 1 {
                    // Interactive login may have replaced the account. Invalidate
                    // before any replacement fetch so an old worker cannot land.
                    app::invalidate(ProviderId::Claude, false);
                }
                let sh = settings_hwnd();
                if !sh.is_invalid() && IsWindowVisible(sh).as_bool() {
                    render_settings(sh);
                }
                if matches!(
                    auth::snapshot().connection,
                    Some(auth::ClaudeConnection::Connected { .. })
                ) {
                    // An explicit login may have replaced the credential that
                    // just failed; do not let the normal debounce delay it.
                    spawn_fetch(ProviderId::Claude, RefreshTrigger::Automatic);
                } else {
                    update_tray(hwnd);
                    render_flyout_current();
                }
                LRESULT(0)
            }
            WM_TOAST_ACTIVATED => {
                // toast clicked: open the flyout anchored at the tray icon
                let id = NOTIFYICONIDENTIFIER {
                    cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
                    hWnd: hwnd,
                    uID: TRAY_ID,
                    ..Default::default()
                };
                let (x, y) = match Shell_NotifyIconGetRect(&id) {
                    Ok(rc) => ((rc.left + rc.right) / 2, (rc.top + rc.bottom) / 2),
                    Err(_) => {
                        let mut pt = POINT::default();
                        let _ = GetCursorPos(&mut pt);
                        (pt.x, pt.y)
                    }
                };
                show_flyout(x, y);
                LRESULT(0)
            }
            WM_UPDATE => {
                if wparam.0 == 1 {
                    // new exe is spawned and waiting on the mutex — hand over
                    let _ = DestroyWindow(hwnd);
                } else {
                    let sh = settings_hwnd();
                    if !sh.is_invalid() && IsWindowVisible(sh).as_bool() {
                        render_settings(sh);
                    }
                    render_flyout_current(); // gear dot
                }
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_POLL => {
                // Also recover queued events if a worker's wakeup was dropped.
                if app::drain_events(Duration::from_secs(u64::from(
                    POLL_SECS.load(Ordering::SeqCst),
                ))) {
                    update_tray(hwnd);
                    render_flyout_current();
                }
                vibecode::reconcile_active_scheme();
                spawn_fetch_all(RefreshTrigger::Automatic);
                updater::maybe_check(); // no-op unless 24h passed
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_TICK => {
                render_flyout_current(); // keep "Updated Xm ago" honest
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                // React only to theme/accent broadcasts — wallpaper changes and
                // random SPI updates also land here and are noise.
                if !demo::is_active() && setting_change_is_theme(lparam) {
                    refresh_theme(hwnd);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_SYSCOLORCHANGE => {
                if !demo::is_active() {
                    refresh_theme(hwnd);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_POWERBROADCAST => {
                if !demo::is_active() {
                    vibecode::reconcile_active_scheme();
                    render_flyout_current();
                    let settings = settings_hwnd();
                    if !settings.is_invalid() && IsWindowVisible(settings).as_bool() {
                        render_settings(settings);
                    }
                }
                LRESULT(1)
            }
            WM_QUERYENDSESSION => {
                if !demo::is_active() {
                    vibecode::restore_for_exit();
                }
                LRESULT(1)
            }
            WM_ENDSESSION => {
                if !demo::is_active() {
                    if wparam.0 != 0 {
                        vibecode::restore_for_exit();
                    } else {
                        vibecode::init();
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                if !demo::is_active() {
                    vibecode::restore_for_exit();
                }
                remove_tray(hwnd);
                PostQuitMessage(0);
                LRESULT(0)
            }
            m if m != 0 && m == TASKBAR_MSG.load(Ordering::SeqCst) => {
                add_tray_icon(hwnd);
                update_tray(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

extern "system" fn flyout_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_GETOBJECT => {
                accessibility::get_object(hwnd, wparam, lparam, accessibility::WindowKind::Flyout)
            }
            accessibility::WM_UIA_QUERY => match wparam.0 {
                accessibility::QUERY_FOCUS => LRESULT(UI.with(|ui| ui.borrow().fly_focus) as isize),
                accessibility::QUERY_SCROLL => {
                    LRESULT(UI.with(|ui| ui.borrow().fly_scroll.to_bits()) as isize)
                }
                _ => LRESULT(0),
            },
            accessibility::WM_UIA_FOCUS => {
                if wparam.0 < FLY_FOCUS_N as usize {
                    UI.with(|ui| ui.borrow_mut().fly_focus = wparam.0 as i32);
                    scroll_flyout_focus_into_view(hwnd);
                    let _ = SetForegroundWindow(hwnd);
                    let _ = SetFocus(hwnd);
                    render_flyout_current();
                    accessibility::focus_changed(hwnd, accessibility::WindowKind::Flyout, wparam.0);
                }
                LRESULT(0)
            }
            accessibility::WM_UIA_INVOKE => {
                if wparam.0 < FLY_FOCUS_N as usize {
                    activate_flyout_control(wparam.0 as i32);
                }
                LRESULT(0)
            }
            WM_ACTIVATE => {
                if !demo::is_active() && (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                    hide_flyout();
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let vk = wparam.0 as u16;
                if vk == VK_ESCAPE.0 {
                    if demo::is_active() {
                        let owner = HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _);
                        let _ = DestroyWindow(owner);
                    } else {
                        hide_flyout();
                    }
                } else if vk == VK_TAB.0 {
                    let back = GetKeyState(VK_SHIFT.0 as i32) < 0;
                    UI.with(|ui| {
                        let mut ui = ui.borrow_mut();
                        ui.fly_focus = if ui.fly_focus < 0 {
                            if back {
                                FLY_FOCUS_N - 1
                            } else {
                                0
                            }
                        } else if back {
                            (ui.fly_focus - 1 + FLY_FOCUS_N) % FLY_FOCUS_N
                        } else {
                            (ui.fly_focus + 1) % FLY_FOCUS_N
                        };
                    });
                    scroll_flyout_focus_into_view(hwnd);
                    let _ = SetFocus(hwnd);
                    render_flyout_current();
                    let focus = UI.with(|ui| ui.borrow().fly_focus);
                    if focus >= 0 {
                        accessibility::focus_changed(
                            hwnd,
                            accessibility::WindowKind::Flyout,
                            focus as usize,
                        );
                    }
                } else if vk == VK_RETURN.0 || vk == VK_SPACE.0 {
                    activate_flyout_control(UI.with(|ui| ui.borrow().fly_focus));
                } else if vk == VK_PRIOR.0 || vk == VK_NEXT.0 {
                    let direction = if vk == VK_PRIOR.0 { -1.0 } else { 1.0 };
                    scroll_flyout_by(hwnd, direction * flyout_viewport_height(hwnd) * 0.8);
                } else if vk == VK_HOME.0 || vk == VK_END.0 {
                    let limit = flyout_scroll_limit(hwnd);
                    UI.with(|ui| {
                        ui.borrow_mut().fly_scroll = if vk == VK_HOME.0 { 0.0 } else { limit }
                    });
                    render_flyout_current();
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = mouse_dip(hwnd, lparam);
                let hover = fly_hit(x, y);
                let changed = UI.with(|ui| {
                    let mut ui = ui.borrow_mut();
                    let changed = ui.fly_hover != hover;
                    ui.fly_hover = hover;
                    if !ui.fly_tracking {
                        track_leave(hwnd);
                        ui.fly_tracking = true;
                    }
                    changed
                });
                if changed {
                    render_flyout_current();
                }
                LRESULT(0)
            }
            MSG_MOUSELEAVE => {
                let changed = UI.with(|ui| {
                    let mut ui = ui.borrow_mut();
                    ui.fly_tracking = false;
                    let changed = ui.fly_hover != gfx::FlyHover::None;
                    ui.fly_hover = gfx::FlyHover::None;
                    changed
                });
                if changed {
                    render_flyout_current();
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = mouse_dip(hwnd, lparam);
                let control = match fly_hit(x, y) {
                    gfx::FlyHover::Refresh => 0,
                    gfx::FlyHover::Gear => 1,
                    gfx::FlyHover::Vibe => 2,
                    gfx::FlyHover::None => -1,
                };
                if control >= 0 {
                    activate_flyout_control(control);
                } else {
                    let view = demo::view().unwrap_or_else(current_view);
                    let scroll = UI.with(|ui| ui.borrow().fly_scroll);
                    if let Some(card) = gfx::row_shortcut(&view, x, y + scroll) {
                        activate_settings_card(
                            HWND(SETTINGS_HWND.load(Ordering::SeqCst) as *mut _),
                            card,
                        );
                    }
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) as u16 as i16) as f32 / 120.0;
                scroll_flyout_by(hwnd, -delta * 48.0);
                LRESULT(0)
            }
            WM_PAINT => {
                let _ = ValidateRect(hwnd, None);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

extern "system" fn settings_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_GETOBJECT => {
                accessibility::get_object(hwnd, wparam, lparam, accessibility::WindowKind::Settings)
            }
            accessibility::WM_UIA_QUERY => match wparam.0 {
                accessibility::QUERY_FOCUS => LRESULT(UI.with(|ui| ui.borrow().set_focus) as isize),
                accessibility::QUERY_SCROLL => {
                    LRESULT(UI.with(|ui| ui.borrow().set_scroll.to_bits()) as isize)
                }
                _ => LRESULT(0),
            },
            accessibility::WM_UIA_FOCUS => {
                if wparam.0 < gfx::N_CARDS {
                    UI.with(|ui| ui.borrow_mut().set_focus = wparam.0 as i32);
                    scroll_settings_focus_into_view(hwnd);
                    let _ = SetForegroundWindow(hwnd);
                    let _ = SetFocus(hwnd);
                    render_settings(hwnd);
                    accessibility::focus_changed(
                        hwnd,
                        accessibility::WindowKind::Settings,
                        wparam.0,
                    );
                }
                LRESULT(0)
            }
            accessibility::WM_UIA_INVOKE => {
                if wparam.0 < gfx::N_CARDS {
                    activate_settings_card(hwnd, wparam.0);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = mouse_dip(hwnd, lparam);
                let hover = settings_hit(x, y);
                let changed = UI.with(|ui| {
                    let mut ui = ui.borrow_mut();
                    let changed = ui.set_hover != hover;
                    ui.set_hover = hover;
                    if !ui.set_tracking {
                        track_leave(hwnd);
                        ui.set_tracking = true;
                    }
                    changed
                });
                if changed {
                    render_settings(hwnd);
                }
                LRESULT(0)
            }
            MSG_MOUSELEAVE => {
                let changed = UI.with(|ui| {
                    let mut ui = ui.borrow_mut();
                    ui.set_tracking = false;
                    let changed = ui.set_hover != -1;
                    ui.set_hover = -1;
                    changed
                });
                if changed {
                    render_settings(hwnd);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = mouse_dip(hwnd, lparam);
                let hit = settings_hit(x, y);
                if hit >= 0 {
                    UI.with(|ui| ui.borrow_mut().set_focus = hit); // focus follows click
                    accessibility::focus_changed(
                        hwnd,
                        accessibility::WindowKind::Settings,
                        hit as usize,
                    );
                }
                if demo::is_active()
                    && hit != gfx::CARD_DIAGNOSTICS as i32
                    && hit != gfx::CARD_QUOTA_DISPLAY as i32
                    && hit != gfx::CARD_RESET_FORMAT as i32
                {
                    render_settings(hwnd);
                    return LRESULT(0);
                }
                if hit == gfx::CARD_INTERVAL as i32 {
                    // pill click selects the interval directly
                    let scroll = UI.with(|ui| ui.borrow().set_scroll);
                    let cards = gfx::settings_rects(scroll);
                    let pills = gfx::interval_pills(&cards[gfx::CARD_INTERVAL]);
                    if let Some(i) = pills.iter().position(|r| contains(r, x, y)) {
                        apply_interval(gfx::INTERVALS[i].0);
                    }
                    render_settings(hwnd);
                } else if hit >= 0 {
                    activate_settings_card(hwnd, hit as usize);
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) as u16 as i16) as f32 / 120.0;
                scroll_settings_by(hwnd, -delta * 48.0);
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let vk = wparam.0 as u16;
                if vk == VK_ESCAPE.0 {
                    if demo::is_active() {
                        let owner = HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _);
                        let _ = DestroyWindow(owner);
                    } else {
                        hide_settings(hwnd);
                    }
                } else if vk == VK_TAB.0 {
                    let back = GetKeyState(VK_SHIFT.0 as i32) < 0;
                    UI.with(|ui| {
                        let mut ui = ui.borrow_mut();
                        let n = gfx::N_CARDS as i32;
                        ui.set_focus = if ui.set_focus < 0 {
                            if back {
                                n - 1
                            } else {
                                0
                            }
                        } else if back {
                            (ui.set_focus - 1 + n) % n
                        } else {
                            (ui.set_focus + 1) % n
                        };
                    });
                    let _ = SetFocus(hwnd);
                    scroll_settings_focus_into_view(hwnd);
                    render_settings(hwnd);
                    let focus = UI.with(|ui| ui.borrow().set_focus);
                    if focus >= 0 {
                        accessibility::focus_changed(
                            hwnd,
                            accessibility::WindowKind::Settings,
                            focus as usize,
                        );
                    }
                } else if vk == VK_PRIOR.0 || vk == VK_NEXT.0 {
                    let direction = if vk == VK_PRIOR.0 { -1.0 } else { 1.0 };
                    scroll_settings_by(hwnd, direction * settings_viewport_height(hwnd) * 0.8);
                } else if vk == VK_HOME.0 || vk == VK_END.0 {
                    let limit = settings_scroll_limit(hwnd);
                    UI.with(|ui| {
                        let mut ui = ui.borrow_mut();
                        ui.set_scroll = if vk == VK_HOME.0 { 0.0 } else { limit };
                        ui.set_hover = -1;
                    });
                    render_settings(hwnd);
                } else if vk == VK_LEFT.0 || vk == VK_RIGHT.0 {
                    if !demo::is_active()
                        && UI.with(|ui| ui.borrow().set_focus) == gfx::CARD_INTERVAL as i32
                    {
                        let dir = if vk == VK_LEFT.0 { -1 } else { 1 };
                        step_interval(dir);
                        render_settings(hwnd);
                    }
                } else if vk == VK_RETURN.0 || vk == VK_SPACE.0 {
                    let f = UI.with(|ui| ui.borrow().set_focus);
                    if f >= 0 {
                        activate_settings_card(hwnd, f as usize);
                    }
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                if demo::is_active() {
                    let owner = HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _);
                    let _ = DestroyWindow(owner);
                } else {
                    hide_settings(hwnd);
                }
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let rc = *(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    HWND::default(),
                    rc.left,
                    rc.top,
                    rc.right - rc.left,
                    rc.bottom - rc.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                clamp_settings_scroll(hwnd);
                render_settings(hwnd);
                LRESULT(0)
            }
            WM_SIZE => {
                if settings_hwnd() == hwnd {
                    clamp_settings_scroll(hwnd);
                    render_settings(hwnd);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let _ = ValidateRect(hwnd, None);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe fn activate_flyout_control(index: i32) {
    match index {
        0 => {
            spawn_fetch_all(RefreshTrigger::Manual);
            render_flyout_current();
        }
        1 => {
            hide_flyout();
            open_settings();
        }
        2 => {
            vibecode::set(!vibecode::is_on());
            render_flyout_current();
        }
        _ => {}
    }
}

// ---------- visibility + resource lifecycle ----------

unsafe fn setting_change_is_theme(lparam: LPARAM) -> bool {
    let p = lparam.0 as *const u16;
    if p.is_null() {
        return false;
    }
    let mut len = 0usize;
    while len < 64 && *p.add(len) != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
    s == "ImmersiveColorSet" || s == "HighContrast"
}

fn ui_contrast() -> Option<util::ContrastColors> {
    match demo::active() {
        Some(state) => state.contrast.then_some(util::DEMO_CONTRAST_COLORS),
        None => util::contrast_colors(),
    }
}

unsafe fn refresh_theme(owner: HWND) {
    update_tray(owner);
    apply_flyout_theme(flyout_hwnd());
    if IsWindowVisible(flyout_hwnd()).as_bool() {
        show_flyout(
            ANCHOR_X.load(Ordering::SeqCst),
            ANCHOR_Y.load(Ordering::SeqCst),
        );
    }
    let settings = settings_hwnd();
    if !settings.is_invalid() {
        apply_settings_theme(settings);
        if IsWindowVisible(settings).as_bool() {
            render_settings(settings);
        }
    }
}

/// Hide the flyout AND drop its whole D3D/D2D/DComp stack — the GPU runtime
/// costs ~40 MB private while resident. Recreated lazily on next show (~10ms).
unsafe fn hide_flyout() {
    let fh = flyout_hwnd();
    let _ = ShowWindow(fh, SW_HIDE);
    let _ = KillTimer(HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _), TIMER_TICK);
    UI.with(|ui| ui.borrow_mut().fly = None);
}

/// Same deal for the settings window.
unsafe fn hide_settings(hwnd: HWND) {
    let _ = ShowWindow(hwnd, SW_HIDE);
    UI.with(|ui| ui.borrow_mut().set = None);
}

// ---------- settings actions (shared by mouse + keyboard) ----------

unsafe fn apply_interval(secs: u32) {
    if config::set_poll_interval_seconds(secs).is_err() {
        return;
    }
    POLL_SECS.store(secs, Ordering::SeqCst);
    let mh = HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _);
    let _ = KillTimer(mh, TIMER_POLL);
    SetTimer(mh, TIMER_POLL, secs * 1000, None);
}

unsafe fn step_interval(dir: i32) {
    let cur = POLL_SECS.load(Ordering::SeqCst);
    let idx = gfx::INTERVALS
        .iter()
        .position(|(s, _)| *s == cur)
        .unwrap_or(1) as i32;
    let next = (idx + dir).clamp(0, gfx::INTERVALS.len() as i32 - 1) as usize;
    apply_interval(gfx::INTERVALS[next].0);
}

unsafe fn activate_settings_card(hwnd: HWND, i: usize) {
    if i == gfx::CARD_DIAGNOSTICS {
        let copied = diagnostics::copy(hwnd).is_ok();
        UI.with_borrow_mut(|ui| ui.diagnostics_copy = if copied { "Copied" } else { "Retry" });
        render_settings(hwnd);
        return;
    }
    if i == gfx::CARD_QUOTA_DISPLAY || i == gfx::CARD_RESET_FORMAT {
        if config::toggle_row_format(i == gfx::CARD_RESET_FORMAT).is_ok() {
            render_flyout_current();
        }
        if !hwnd.is_invalid() && IsWindowVisible(hwnd).as_bool() {
            render_settings(hwnd);
        }
        return;
    }
    if demo::is_active() {
        return;
    }
    match i {
        gfx::CARD_ACCOUNT => activate_claude_account(),
        gfx::CARD_CAPS => match util::caps_led_state() {
            util::CapsLedState::InstalledDisabled => {
                let _ = util::set_caps_led_enabled(true);
            }
            util::CapsLedState::InstalledEnabled => {
                let _ = util::set_caps_led_enabled(false);
            }
            util::CapsLedState::Error(_) => {
                let _ = util::retry_caps_led();
            }
            util::CapsLedState::Unavailable => {}
        },
        gfx::CARD_AUTOSTART => util::set_autostart(!util::autostart_enabled()),
        gfx::CARD_CODEX => {
            let on = !config::settings().codex_enabled;
            if config::set_codex_enabled(on).is_ok() {
                if on {
                    spawn_fetch(ProviderId::Codex, RefreshTrigger::Manual);
                } else {
                    app::invalidate(ProviderId::Codex, true);
                }
                update_tray(HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _));
            }
        }
        gfx::CARD_ALERTS => {
            let enabled = !config::settings().alerts_enabled;
            let _ = config::set_alerts_enabled(enabled);
        }
        gfx::CARD_PACE => {
            if config::set_pace_colors_enabled(!config::settings().pace_colors_enabled).is_ok() {
                render_flyout_current();
            }
        }
        gfx::CARD_CODEX_SERVER => {
            if config::set_codex_app_server_enabled(!config::settings().codex_app_server_enabled)
                .is_ok()
            {
                app::invalidate(ProviderId::Codex, !config::settings().codex_enabled);
                if config::settings().codex_enabled {
                    spawn_fetch(ProviderId::Codex, RefreshTrigger::Manual);
                }
            }
        }
        gfx::CARD_UPDATE_CHECKS => {
            let enabled = !config::settings().update_checks_enabled;
            if config::set_update_checks_enabled(enabled).is_ok() && enabled {
                updater::maybe_check();
            }
        }
        gfx::CARD_LID => {
            match vibecode::persistent_status() {
                vibecode::PersistentStatus::LegacyRecoveryPending => {
                    vibecode::restore_legacy_to_current_scheme();
                }
                vibecode::PersistentStatus::RecoveryRequired => {
                    vibecode::retry_recovery();
                }
                vibecode::PersistentStatus::Error if vibecode::persistent_preference_enabled() => {
                    vibecode::set_persistent_override(false);
                }
                status => {
                    vibecode::set_persistent_override(
                        status != vibecode::PersistentStatus::Applied,
                    );
                }
            }
            render_flyout_current();
        }
        gfx::CARD_INTERVAL => {
            // keyboard activate on the interval card: cycle to the next option
            let cur = POLL_SECS.load(Ordering::SeqCst);
            let idx = gfx::INTERVALS
                .iter()
                .position(|(s, _)| *s == cur)
                .unwrap_or(1);
            apply_interval(gfx::INTERVALS[(idx + 1) % gfx::INTERVALS.len()].0);
        }
        gfx::CARD_REFRESH => spawn_fetch_all(RefreshTrigger::Manual),
        gfx::CARD_ABOUT => match updater::status() {
            updater::Status::Available(_) if updater::can_self_update() => updater::install(),
            updater::Status::Available(release) => updater::open_url(&release.page_url),
            updater::Status::Installing => {}
            updater::Status::Failed(_, page) => {
                updater::open_url(page.as_deref().unwrap_or(network::GITHUB_REPOSITORY_URL));
            }
            updater::Status::UpToDate => updater::open_url(network::GITHUB_REPOSITORY_URL),
        },
        gfx::CARD_QUIT => {
            let _ = DestroyWindow(HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _));
            return;
        }
        _ => {}
    }
    render_settings(hwnd);
}

fn activate_claude_account() {
    let snap = auth::snapshot();
    if snap.busy {
        // Sign-in runs in a console the user drives; a second click abandons a
        // flow they've given up on instead of doing nothing for ten minutes.
        auth::cancel_interactive();
        return;
    }
    match snap.connection {
        Some(auth::ClaudeConnection::CliUnavailable) => {
            updater::open_url(network::CLAUDE_CODE_GETTING_STARTED_URL);
        }
        // An initial click retries status; every known account state starts
        // the official browser flow so expired and switched accounts recover.
        None => spawn_claude_account(false),
        Some(_) => spawn_claude_account(true),
    }
}

// ---------- hit testing ----------

unsafe fn mouse_dip(hwnd: HWND, lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
    let dpi = GetDpiForWindow(hwnd) as f32;
    let scale = dpi / 96.0;
    (x as f32 / scale, y as f32 / scale)
}

fn contains(r: &windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x <= r.right && y >= r.top && y <= r.bottom
}

fn fly_hit(x: f32, y: f32) -> gfx::FlyHover {
    let scroll = UI.with(|ui| ui.borrow().fly_scroll);
    let y = y + scroll;
    let (refresh, gear) = gfx::fly_btns();
    if contains(&refresh, x, y) {
        gfx::FlyHover::Refresh
    } else if contains(&gear, x, y) {
        gfx::FlyHover::Gear
    } else if contains(
        &gfx::vibe_row_at(UI.with(|ui| ui.borrow().fly_vibe_top)),
        x,
        y,
    ) {
        gfx::FlyHover::Vibe
    } else {
        gfx::FlyHover::None
    }
}

unsafe fn flyout_viewport_height(hwnd: HWND) -> f32 {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let dpi = GetDpiForWindow(hwnd).max(96) as f32;
    (rc.bottom - rc.top) as f32 / (dpi / 96.0)
}

unsafe fn flyout_scroll_limit(hwnd: HWND) -> f32 {
    let view = demo::view().unwrap_or_else(current_view);
    (gfx::flyout_height(&view) - flyout_viewport_height(hwnd)).max(0.0)
}

unsafe fn scroll_flyout_by(hwnd: HWND, delta: f32) {
    let limit = flyout_scroll_limit(hwnd);
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.fly_scroll = (ui.fly_scroll + delta).clamp(0.0, limit);
        ui.fly_hover = gfx::FlyHover::None;
    });
    render_flyout_current();
}

unsafe fn scroll_flyout_focus_into_view(hwnd: HWND) {
    let viewport = flyout_viewport_height(hwnd);
    let limit = flyout_scroll_limit(hwnd);
    let (refresh, settings) = gfx::fly_btns();
    let view = demo::view().unwrap_or_else(current_view);
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        let rect = match ui.fly_focus {
            0 => refresh,
            1 => settings,
            2 => gfx::vibe_row(&view),
            _ => return,
        };
        if rect.top < ui.fly_scroll + 8.0 {
            ui.fly_scroll = rect.top - 8.0;
        } else if rect.bottom > ui.fly_scroll + viewport - 8.0 {
            ui.fly_scroll = rect.bottom - viewport + 8.0;
        }
        ui.fly_scroll = ui.fly_scroll.clamp(0.0, limit);
    });
}

fn settings_hit(x: f32, y: f32) -> i32 {
    let scroll = UI.with(|ui| ui.borrow().set_scroll);
    for (i, card) in gfx::settings_rects(scroll).iter().enumerate() {
        if contains(card, x, y) {
            return i as i32;
        }
    }
    -1
}

unsafe fn settings_viewport_height(hwnd: HWND) -> f32 {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let dpi = GetDpiForWindow(hwnd).max(96) as f32;
    (rc.bottom - rc.top) as f32 / (dpi / 96.0)
}

unsafe fn settings_scroll_limit(hwnd: HWND) -> f32 {
    (gfx::settings_height() - settings_viewport_height(hwnd)).max(0.0)
}

unsafe fn clamp_settings_scroll(hwnd: HWND) {
    let limit = settings_scroll_limit(hwnd);
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.set_scroll = ui.set_scroll.clamp(0.0, limit);
    });
}

unsafe fn scroll_settings_by(hwnd: HWND, delta: f32) {
    let limit = settings_scroll_limit(hwnd);
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.set_scroll = (ui.set_scroll + delta).clamp(0.0, limit);
        ui.set_hover = -1;
    });
    render_settings(hwnd);
}

unsafe fn scroll_settings_focus_into_view(hwnd: HWND) {
    let viewport = settings_viewport_height(hwnd);
    let limit = settings_scroll_limit(hwnd);
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        if ui.set_focus < 0 {
            return;
        }
        let card = gfx::settings_rects(0.0)[ui.set_focus as usize];
        if card.top < ui.set_scroll + 8.0 || ui.set_focus == gfx::CARD_DIAGNOSTICS as i32 {
            ui.set_scroll = card.top - 8.0;
        } else if card.bottom > ui.set_scroll + viewport - 8.0 {
            ui.set_scroll = card.bottom - viewport + 8.0;
        }
        ui.set_scroll = ui.set_scroll.clamp(0.0, limit);
    });
}

unsafe fn track_leave(hwnd: HWND) {
    let mut tme = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    let _ = TrackMouseEvent(&mut tme);
}

// ---------- flyout ----------

unsafe fn style_flyout(h: HWND) {
    let corner = DWMWCP_ROUND;
    let _ = DwmSetWindowAttribute(
        h,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        &corner as *const _ as *const _,
        std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
    );
    // DWMSBT_TRANSIENTWINDOW only renders its opaque fallback on this
    // borderless DComp popup — accent-policy acrylic instead (util).
    if let Some(state) = demo::active() {
        let dark = !state.light;
        let dark_bool = BOOL(if dark { 1 } else { 0 });
        let _ = DwmSetWindowAttribute(
            h,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_bool as *const _ as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );
        util::apply_acrylic(h, dark, state.contrast);
    } else {
        apply_flyout_theme(h);
    }
}

unsafe fn apply_flyout_theme(h: HWND) {
    let dark = util::is_dark_theme();
    let dark_bool = BOOL(if dark { 1 } else { 0 });
    let _ = DwmSetWindowAttribute(
        h,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        &dark_bool as *const _ as *const _,
        std::mem::size_of::<BOOL>() as u32,
    );
    util::apply_acrylic(h, dark, ui_contrast().is_some());
}

/// Last good snapshot survives transient errors (429, network blips):
/// the flyout and tray keep showing stale data; the whole-flyout error view
/// only appears when nothing was ever fetched from any provider. Per-provider
/// failures degrade to a dim note line inside that provider's section.
fn current_view() -> gfx::View {
    let (c_snap, mut c_err) = effective(ProviderId::Claude);
    let interval = Duration::from_secs(u64::from(POLL_SECS.load(Ordering::SeqCst)));
    if let Some((short, detail)) = app::error_text(ProviderId::Claude, interval) {
        c_err = Some(format!("{short}\n{detail}"));
    }
    let codex_on = codex_active();
    let refresh_note = join_notes(app::cached_notes(), manual_cooldown_note());

    // Claude-only path — identical to the single-provider behavior
    if !codex_on {
        return match (c_snap, c_err) {
            (None, None) => gfx::View::Loading,
            (None, Some(mut msg)) => {
                if let Some(note) = refresh_note {
                    msg.push('\n');
                    msg.push_str(&note);
                }
                gfx::View::Error(msg)
            }
            (Some(s), err) => gfx::View::Data(gfx::FlyoutData {
                fetched_unix: Some(s.fetched_unix),
                note: join_notes(err.as_deref().map(err_head), refresh_note),
                sections: vec![section("Claude", s)],
            }),
        };
    }

    let (x_snap, mut x_err) = effective(ProviderId::Codex);
    if let Some((short, detail)) = app::error_text(ProviderId::Codex, interval) {
        x_err = Some(format!("{short}\n{detail}"));
    }
    if c_snap.is_none() && c_err.is_none() && x_snap.is_none() && x_err.is_none() {
        return gfx::View::Loading;
    }

    let mut sections = Vec::new();
    let mut notes = Vec::new();
    let mut fetched: Option<i64> = None;
    for (title, snap, err) in [("Claude", c_snap, c_err), ("Codex", x_snap, x_err)] {
        match (snap, err) {
            (Some(s), err) => {
                // footer shows the OLDEST data on screen — a fresh Codex fetch
                // must not say "Updated just now" over stale Claude rows
                fetched = Some(fetched.map_or(s.fetched_unix, |f| f.min(s.fetched_unix)));
                if err.is_some() {
                    notes.push(format!(
                        "{title}: {}",
                        err.as_deref().map(err_head).unwrap_or_default()
                    ));
                }
                sections.push(section(title, s));
            }
            (None, Some(msg)) => sections.push(gfx::Section {
                title,
                plan: String::new(),
                status: None,
                body: gfx::SectionBody::Note(
                    msg.lines().next().unwrap_or("Can't load usage").to_string(),
                ),
            }),
            (None, None) => sections.push(gfx::Section {
                title,
                plan: String::new(),
                status: None,
                body: gfx::SectionBody::Note("Loading…".to_string()),
            }),
        }
    }
    if let Some(note) = refresh_note {
        notes.push(note);
    }
    gfx::View::Data(gfx::FlyoutData {
        sections,
        fetched_unix: fetched,
        note: if notes.is_empty() {
            None
        } else {
            Some(notes.join(" · "))
        },
    })
}

fn join_notes(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(first), Some(second)) => Some(format!("{first} · {second}")),
        (Some(note), None) | (None, Some(note)) => Some(note),
        (None, None) => None,
    }
}

fn manual_cooldown_deadlines() -> Vec<(ProviderId, i64)> {
    app::manual_cooldown_deadlines(Duration::from_secs(u64::from(
        POLL_SECS.load(Ordering::SeqCst),
    )))
}
fn manual_cooldown_note() -> Option<String> {
    let notices: Vec<_> = manual_cooldown_deadlines()
        .into_iter()
        .map(|(provider, deadline)| {
            format!(
                "{} refresh available at {}",
                match provider {
                    ProviderId::Claude => "Claude",
                    ProviderId::Codex => "Codex",
                },
                api::fmt_unix_hhmm(deadline)
            )
        })
        .collect();
    (!notices.is_empty()).then(|| notices.join(" · "))
}

fn manual_refresh_label() -> String {
    manual_cooldown_deadlines()
        .into_iter()
        .map(|(_, deadline)| deadline)
        .min()
        .map(|deadline| format!("Refresh available at {}", api::fmt_unix_hhmm(deadline)))
        .unwrap_or_else(|| "Refresh usage now".to_string())
}

fn section(title: &'static str, s: UsageSnapshot) -> gfx::Section {
    let now = SystemClock.read().unix_seconds;
    let pace_enabled = config::settings().pace_colors_enabled;
    gfx::Section {
        title,
        plan: s.plan.unwrap_or_default(),
        status: app::error_text(
            s.provider,
            Duration::from_secs(u64::from(POLL_SECS.load(Ordering::SeqCst))),
        )
        .map(|(short, _)| short)
        .or_else(|| {
            s.reset_credits_available
                .map(|count| format!("Reset credits available: {count}"))
        }),
        body: gfx::SectionBody::Rows(
            s.rows
                .into_iter()
                .map(|row| gfx::LimitRow::with_pace(row, s.provider, now, pace_enabled))
                .collect(),
        ),
    }
}

/// First line, no trailing period — footer-note form of an error message.
fn err_head(msg: &str) -> String {
    msg.lines()
        .next()
        .unwrap_or("couldn't update")
        .trim_end_matches('.')
        .to_string()
}

unsafe fn toggle_flyout(x: i32, y: i32) {
    let fh = flyout_hwnd();
    if IsWindowVisible(fh).as_bool() {
        hide_flyout();
    } else {
        show_flyout(x, y);
    }
}

unsafe fn show_flyout(cx: i32, cy: i32) {
    if let Some(state) = demo::active() {
        show_demo_flyout(flyout_hwnd(), state);
        return;
    }
    ANCHOR_X.store(cx, Ordering::SeqCst);
    ANCHOR_Y.store(cy, Ordering::SeqCst);
    let fh = flyout_hwnd();
    let was_visible = IsWindowVisible(fh).as_bool();
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.fly_hover = gfx::FlyHover::None;
        if !was_visible {
            ui.fly_focus = -1;
            ui.fly_scroll = 0.0;
        }
    });

    spawn_fetch(ProviderId::Claude, RefreshTrigger::Flyout);
    if codex_active() {
        spawn_fetch(ProviderId::Codex, RefreshTrigger::Flyout);
    }

    let view = current_view();

    let pt = POINT { x: cx, y: cy };
    let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
    let mut dpix: u32 = 96;
    let mut dpiy: u32 = 96;
    let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpix, &mut dpiy);
    let dpi = dpix as f32;
    let scale = dpi / 96.0;

    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = GetMonitorInfoW(hmon, &mut mi);
    let work = mi.rcWork;

    let w_px = (gfx::FLYOUT_W * scale).round() as i32;
    let margin = (12.0 * scale).round() as i32;
    let h_px = (gfx::flyout_height(&view) * scale)
        .round()
        .min((gfx::FLYOUT_MAX_H * scale).round())
        .min((work.bottom - work.top - 2 * margin).max(1) as f32) as i32;
    let max_scroll = (gfx::flyout_height(&view) - h_px as f32 / scale).max(0.0);
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.fly_scroll = ui.fly_scroll.clamp(0.0, max_scroll);
    });
    let x = (cx - w_px / 2)
        .max(work.left + margin)
        .min(work.right - w_px - margin);
    let y = if cy > (work.top + work.bottom) / 2 {
        work.bottom - h_px - margin
    } else {
        work.top + margin
    };

    let _ = SetWindowPos(fh, HWND_TOPMOST, x, y, w_px, h_px, SWP_NOACTIVATE);
    render_flyout(fh, &view, w_px as u32, h_px as u32, dpi);
    let _ = ShowWindow(fh, SW_SHOW);
    let _ = SetForegroundWindow(fh);
    // relative "Updated…" label tick — lives only while visible
    SetTimer(
        HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _),
        TIMER_TICK,
        30_000,
        None,
    );
}

unsafe fn show_demo_flyout(fh: HWND, state: &demo::State) {
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.fly_hover = gfx::FlyHover::None;
        ui.fly_focus = -1;
        ui.fly_scroll = 0.0;
    });

    let monitor = MonitorFromWindow(fh, MONITOR_DEFAULTTONEAREST);
    let mut dpi_x = 96;
    let mut dpi_y = 96;
    let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    let dpi = dpi_x as f32;
    let scale = dpi / 96.0;
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = GetMonitorInfoW(monitor, &mut info);
    let width = (gfx::FLYOUT_W * scale).round() as i32;
    let height = (gfx::flyout_height(&state.view) * scale)
        .round()
        .min((gfx::FLYOUT_MAX_H * scale).round())
        .min((info.rcWork.bottom - info.rcWork.top - (24.0 * scale).round() as i32).max(1) as f32)
        as i32;
    let x = info.rcWork.left + (info.rcWork.right - info.rcWork.left - width) / 2;
    let y = info.rcWork.top + (info.rcWork.bottom - info.rcWork.top - height) / 2;

    let _ = SetWindowPos(fh, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE);
    render_demo_flyout(fh, state, width as u32, height as u32, dpi);
    let _ = ShowWindow(fh, SW_SHOW);
    let _ = SetForegroundWindow(fh);
    SetTimer(
        HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _),
        TIMER_TICK,
        30_000,
        None,
    );
}

unsafe fn signal_demo_ready(state: &demo::State) {
    let Some(name) = state.ready_event.as_deref() else {
        return;
    };
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(wide.as_ptr())) {
        let _ = SetEvent(event);
        let _ = CloseHandle(event);
    }
}

unsafe fn render_demo_flyout(fh: HWND, _state: &demo::State, width: u32, height: u32, dpi: f32) {
    let view = demo::view().expect("active demo view");
    render_flyout(fh, &view, width, height, dpi);
}

unsafe fn render_flyout(fh: HWND, view: &gfx::View, w_px: u32, h_px: u32, dpi: f32) {
    let demo = demo::active();
    let dark = demo.map_or_else(util::is_dark_theme, |state| !state.light);
    let accent = demo.map_or_else(util::accent_rgb, |_| (96, 159, 255));
    let contrast = ui_contrast();
    let fetching = demo.map_or_else(any_fetching, |state| state.fetching);
    let vibe_on = demo.is_none() && vibecode::is_on();
    let update_dot = demo.is_none() && updater::has_update();
    let caption = if demo.is_some() {
        "Off · demo mode makes no system changes"
    } else {
        vibecode::flyout_caption()
    };
    update_error_tooltip(fh);
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        if ui.fly.is_none() {
            ui.fly = diagnostics::observe(gfx::Surface::new(fh), "render_init_failed").ok();
        }
        ui.fly_vibe_top = gfx::vibe_row(view).top; // hit-test cache
        let hover = ui.fly_hover;
        let focus = ui.fly_focus;
        let scroll = ui.fly_scroll;
        if let Some(fx) = ui.fly.as_mut() {
            let _ = diagnostics::observe(
                fx.render_flyout(
                    w_px, h_px, dpi, view, dark, accent, contrast, scroll, hover, focus, fetching,
                    update_dot, vibe_on, caption,
                ),
                "render_failed",
            );
        }
    });
}

unsafe fn update_error_tooltip(flyout: HWND) {
    let detail = app::error_details(Duration::from_secs(u64::from(
        POLL_SECS.load(Ordering::SeqCst),
    )))
    .unwrap_or_default();
    UI.with_borrow_mut(|ui| {
        let text: Vec<u16> = detail.encode_utf16().chain(std::iter::once(0)).collect();
        if ui.error_tooltip_text == text {
            return;
        }
        ui.error_tooltip_text = text;
        let newly_created = ui.error_tooltip.is_none();
        if newly_created && !detail.is_empty() {
            ui.error_tooltip = create_window(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                TOOLTIPS_CLASSW,
                PCWSTR::null(),
                WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
                [0, 0, 0, 0],
                flyout,
                HINSTANCE::default(),
            )
            .ok();
        }
        if let Some(tooltip) = ui.error_tooltip {
            let tool = TTTOOLINFOW {
                // The native control accepts the V2 size on this Win32 app.
                cbSize: (std::mem::size_of::<TTTOOLINFOW>()
                    - std::mem::size_of::<*mut std::ffi::c_void>()) as u32,
                uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                hwnd: flyout,
                uId: flyout.0 as usize,
                lpszText: PWSTR(ui.error_tooltip_text.as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(tooltip, TTM_SETMAXTIPWIDTH, WPARAM(0), LPARAM(360));
            SendMessageW(
                tooltip,
                if newly_created {
                    TTM_ADDTOOLW
                } else {
                    TTM_UPDATETIPTEXTW
                },
                WPARAM(0),
                LPARAM(std::ptr::from_ref(&tool) as isize),
            );
        }
    });
}

/// Re-render the flyout at its current size (hover/fetch/tick changes).
unsafe fn render_flyout_current() {
    let fh = flyout_hwnd();
    if !IsWindowVisible(fh).as_bool() {
        return;
    }
    let mut rc = RECT::default();
    let _ = GetClientRect(fh, &mut rc);
    let dpi = GetDpiForWindow(fh) as f32;
    if let Some(state) = demo::active() {
        render_demo_flyout(
            fh,
            state,
            (rc.right - rc.left) as u32,
            (rc.bottom - rc.top) as u32,
            dpi,
        );
        return;
    }
    let view = current_view();
    render_flyout(
        fh,
        &view,
        (rc.right - rc.left) as u32,
        (rc.bottom - rc.top) as u32,
        dpi,
    );
}

// ---------- settings window ----------

unsafe fn open_settings() {
    if demo::active().is_some_and(|state| state.scenario != demo::Scenario::Settings) {
        return;
    }
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.set_focus = -1;
        ui.set_scroll = 0.0;
    });
    if !demo::is_active() && auth::snapshot().connection.is_none() {
        spawn_claude_account(false);
    }
    let existing = settings_hwnd();
    if !existing.is_invalid() {
        render_settings(existing);
        let _ = ShowWindow(existing, SW_SHOW);
        let _ = SetForegroundWindow(existing);
        return;
    }

    let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();

    // size for the monitor under the cursor
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
    let mut dpix: u32 = 96;
    let mut dpiy: u32 = 96;
    let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpix, &mut dpiy);
    let scale = dpix as f32 / 96.0;

    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let client_w = (gfx::SET_W * scale).round() as i32;
    let client_h = (gfx::settings_height().min(704.0) * scale).round() as i32;
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: client_w,
        bottom: client_h,
    };
    let _ = AdjustWindowRectExForDpi(&mut rc, style, false, WS_EX_NOREDIRECTIONBITMAP, dpix);
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;

    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = GetMonitorInfoW(hmon, &mut mi);
    let margin = (16.0 * scale).round() as i32;
    let h = h.min((mi.rcWork.bottom - mi.rcWork.top - 2 * margin).max(1));
    let x = mi.rcWork.left + (mi.rcWork.right - mi.rcWork.left - w) / 2;
    let y = mi.rcWork.top + (mi.rcWork.bottom - mi.rcWork.top - h) / 2;

    let Ok(hwnd) = create_window(
        WS_EX_NOREDIRECTIONBITMAP,
        w!("Claudometer.Settings"),
        w!("Claudometer"),
        style,
        [x, y, w, h],
        HWND::default(),
        hinst,
    ) else {
        return;
    };
    SETTINGS_HWND.store(hwnd.0 as isize, Ordering::SeqCst);

    apply_settings_theme(hwnd);
    render_settings(hwnd);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
}

unsafe fn apply_settings_theme(h: HWND) {
    let contrast = ui_contrast().is_some();
    // Mica over the whole window ("sheet of glass" + main-window backdrop)
    let margins = MARGINS {
        cxLeftWidth: if contrast { 0 } else { -1 },
        cxRightWidth: if contrast { 0 } else { -1 },
        cyTopHeight: if contrast { 0 } else { -1 },
        cyBottomHeight: if contrast { 0 } else { -1 },
    };
    let _ = DwmExtendFrameIntoClientArea(h, &margins);
    let backdrop = if contrast {
        DWMSBT_NONE
    } else {
        DWMSBT_MAINWINDOW
    };
    let _ = DwmSetWindowAttribute(
        h,
        DWMWA_SYSTEMBACKDROP_TYPE,
        &backdrop as *const _ as *const _,
        std::mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
    );
    let dark = BOOL(
        if demo::active().map_or_else(util::is_dark_theme, |state| !state.light) {
            1
        } else {
            0
        },
    );
    let _ = DwmSetWindowAttribute(
        h,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        &dark as *const _ as *const _,
        std::mem::size_of::<BOOL>() as u32,
    );
}

unsafe fn render_settings(hwnd: HWND) {
    if hwnd.is_invalid() {
        return;
    }
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let dpi = GetDpiForWindow(hwnd) as f32;
    let demo = demo::active();
    let dark = demo.map_or_else(util::is_dark_theme, |state| !state.light);
    let accent = demo.map_or_else(util::accent_rgb, |_| (96, 159, 255));
    let contrast = ui_contrast();
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        if ui.set.is_none() {
            ui.set = diagnostics::observe(gfx::Surface::new(hwnd), "render_init_failed").ok();
        }
        let mut st = settings_view(ui.set_hover, ui.set_focus, ui.diagnostics_copy);
        st.diagnostics_copy = ui.diagnostics_copy;
        let scroll = ui.set_scroll;
        if let Some(surface) = ui.set.as_mut() {
            let _ = diagnostics::observe(
                surface.render_settings(
                    (rc.right - rc.left) as u32,
                    (rc.bottom - rc.top) as u32,
                    dpi,
                    &st,
                    dark,
                    accent,
                    contrast,
                    scroll,
                ),
                "render_failed",
            );
        }
    });
}

#[inline(never)]
fn settings_view(hover: i32, focus: i32, copy: &'static str) -> gfx::SettingsView {
    if demo::is_active() {
        demo::settings_view(hover, focus)
    } else {
        live_settings_view(hover, focus, copy)
    }
}

fn live_settings_view(hover: i32, focus: i32, copy: &'static str) -> gfx::SettingsView {
    let (mut about, about_btn) = match updater::status() {
        updater::Status::UpToDate => (
            concat!("Claudometer ", env!("CARGO_PKG_VERSION")).to_string(),
            "GitHub",
        ),
        updater::Status::Available(r) if updater::can_self_update() => {
            (format!("Update {} available", r.tag), "Install")
        }
        updater::Status::Available(r) => (
            format!("Update {} — use signed installer/Winget", r.tag),
            "Release",
        ),
        updater::Status::Installing => ("Installing update…".to_string(), "…"),
        updater::Status::Failed(msg, _) => (format!("Update failed — {msg}"), "Release"),
    };
    if let Some(diagnostic) = config::diagnostic()
        .or_else(runtime_state::diagnostic)
        .or_else(vibecode::diagnostic)
    {
        about = diagnostic;
    }
    let (lid_label, lid_caption, lid_action) = match vibecode::persistent_status() {
        vibecode::PersistentStatus::LegacyRecoveryPending => (
            "Advanced · restore legacy lid values",
            "Saved values ready to restore",
            Some("Restore"),
        ),
        vibecode::PersistentStatus::RecoveryRequired => (
            "Advanced · lid recovery required",
            "Restore previous settings to continue",
            Some("Recover"),
        ),
        vibecode::PersistentStatus::Error => (
            "Advanced · lid override unavailable",
            "No lid override is active",
            Some(if vibecode::persistent_preference_enabled() {
                "Disable"
            } else {
                "Retry"
            }),
        ),
        vibecode::PersistentStatus::Applied => (
            "Advanced · ignore lid close",
            "Active · restores on exit",
            None,
        ),
        vibecode::PersistentStatus::Disabled => (
            "Advanced · ignore lid close",
            "Restores on exit when enabled",
            None,
        ),
    };
    let account = auth::snapshot();
    let (account_caption, account_action, account_connected) = if account.busy {
        ("Finish sign-in in the console".to_string(), "Cancel", false)
    } else {
        match account.connection {
            Some(auth::ClaudeConnection::Connected { plan }) => {
                let caption = if plan.is_empty() {
                    "Connected account".to_string()
                } else {
                    format!("Connected account · {plan}")
                };
                (caption, "Reconnect", true)
            }
            Some(auth::ClaudeConnection::Disconnected) => {
                ("Not connected".to_string(), "Connect", false)
            }
            Some(auth::ClaudeConnection::CliUnavailable) => {
                ("Claude Code is required".to_string(), "Install", false)
            }
            Some(auth::ClaudeConnection::Problem(msg)) => (msg, "Reconnect", false),
            None => ("Checking connection…".to_string(), "…", false),
        }
    };
    let (caps_caption, caps_control) = match util::caps_led_state() {
        util::CapsLedState::Unavailable => (
            "Unavailable · hook isn't installed".to_string(),
            gfx::CapsControl::Unavailable,
        ),
        util::CapsLedState::InstalledDisabled => (
            "Installed · disabled".to_string(),
            gfx::CapsControl::Toggle(false),
        ),
        util::CapsLedState::InstalledEnabled => (
            "Installed · enabled".to_string(),
            gfx::CapsControl::Toggle(true),
        ),
        util::CapsLedState::Error(error) => (
            format!("Error · {}", error.message()),
            gfx::CapsControl::Retry,
        ),
    };
    let settings = config::settings();
    gfx::SettingsView {
        diagnostics: diagnostics::text(),
        diagnostics_copy: copy,
        account_caption,
        account_action,
        account_connected,
        caps_caption,
        caps_control,
        autostart: util::autostart_enabled(),
        codex_on: settings.codex_enabled,
        codex_server_on: settings.codex_app_server_enabled,
        pace_on: settings.pace_colors_enabled,
        reset_format: settings.reset_format,
        quota_display: settings.quota_display,
        alerts_on: settings.alerts_enabled,
        update_checks_on: settings.update_checks_enabled,
        lid_label: lid_label.to_string(),
        lid_caption: lid_caption.to_string(),
        lid_on: vibecode::persistent_status() == vibecode::PersistentStatus::Applied,
        lid_action,
        about,
        about_btn,
        update_ready: updater::has_update(),
        poll_secs: POLL_SECS.load(Ordering::SeqCst),
        refresh_label: manual_refresh_label(),
        hover,
        focus,
    }
}

fn spawn_claude_account(login: bool) {
    if demo::is_active() {
        return;
    }
    if !auth::begin_interactive() {
        return;
    }
    let h = MAIN_HWND.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        let connection = if login {
            auth::login()
        } else {
            auth::query_status()
        };
        auth::finish_interactive(connection);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(
                    HWND(h as *mut _),
                    WM_AUTH_READY,
                    WPARAM(usize::from(login)),
                    LPARAM(0),
                );
            }
        }
    });
}

// ---------- tray ----------

unsafe fn base_nid(owner: HWND) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: owner,
        uID: TRAY_ID,
        ..Default::default()
    };
    nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    nid
}

fn set_wstr(dst: &mut [u16], s: &str) {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let n = wide.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&wide[..n]);
    dst[n] = 0;
}

fn set_tip(nid: &mut NOTIFYICONDATAW, s: &str) {
    set_wstr(&mut nid.szTip, s);
}

/// Legacy balloon fallback — only when the WinRT toast path errors out
/// (still renders as a toast on Windows 11, just with fewer niceties).
pub(crate) fn tray_balloon(title: &str, msg: &str) {
    unsafe {
        let h = MAIN_HWND.load(Ordering::SeqCst);
        if h == 0 {
            return;
        }
        let mut nid = base_nid(HWND(h as *mut _));
        nid.uFlags = NIF_INFO | NIF_SHOWTIP;
        nid.dwInfoFlags = NIIF_WARNING | NIIF_RESPECT_QUIET_TIME;
        set_wstr(&mut nid.szInfoTitle, title);
        set_wstr(&mut nid.szInfo, msg);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

unsafe fn add_tray_icon(owner: HWND) {
    if let Some(state) = demo::active() {
        add_demo_tray_icon(owner, state);
        return;
    }
    let dark = util::is_dark_theme();
    let icon = trayicon::build(&trayicon::Style::Loading, dark).unwrap_or_default();
    let mut nid = base_nid(owner);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = icon;
    set_tip(&mut nid, "Claude — loading usage…");
    let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
    swap_prev_icon(icon);
}

unsafe fn add_demo_tray_icon(owner: HWND, state: &demo::State) {
    let style = if let Some(frac) = state.tray_percent {
        trayicon::Style::Ring {
            frac,
            rgb: (96, 159, 255),
        }
    } else if matches!(&state.view, gfx::View::Error(_)) {
        trayicon::Style::Alert
    } else {
        trayicon::Style::Loading
    };
    let icon = trayicon::build(&style, true).unwrap_or_default();
    let mut notification = base_nid(owner);
    notification.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    notification.uCallbackMessage = WM_TRAY;
    notification.hIcon = icon;
    set_tip(&mut notification, state.tray_tip);
    let _ = Shell_NotifyIconW(NIM_ADD, &notification);
    let _ = Shell_NotifyIconW(NIM_SETVERSION, &notification);
    swap_prev_icon(icon);
}

unsafe fn update_tray(owner: HWND) {
    if demo::is_active() {
        return;
    }
    let dark = util::is_dark_theme();
    let accent = util::accent_rgb();

    // ring + first tooltip line stay Claude's (stale data beats an error icon)
    let (c_snap, c_err) = effective(ProviderId::Claude);
    let (style, mut tip) = match (&c_snap, &c_err) {
        (Some(s), _) => {
            let session = s.rows.iter().find(|r| r.kind == LimitKind::Session);
            let weekly = s.rows.iter().find(|r| r.kind == LimitKind::Weekly);
            let (frac, rgb, mut tip) = match session {
                Some(row) => (
                    (row.percent.get() / 100.0) as f32,
                    util::severity_rgb(&row.severity, row.percent.get(), accent),
                    format!("Claude · Session {:.0}%", row.percent.get()),
                ),
                None => (0.0, accent, "Claude".to_string()),
            };
            if let Some(wk) = weekly {
                tip.push_str(&format!(" · Week {:.0}%", wk.percent.get()));
            }
            if let Some(row) = session {
                let reset = row.resets_unix.map(api::fmt_reset_unix).unwrap_or_default();
                if !reset.is_empty() {
                    tip.push_str(&format!(" · {reset}"));
                }
            }
            (trayicon::Style::Ring { frac, rgb }, tip)
        }
        (None, Some(msg)) => (
            trayicon::Style::Alert,
            format!("Claude — {}", msg.replace('\n', " ")),
        ),
        (None, None) => (
            trayicon::Style::Loading,
            "Claude — loading usage…".to_string(),
        ),
    };

    if codex_active() {
        match effective(ProviderId::Codex) {
            (Some(s), _) => {
                let mut line = "Codex".to_string();
                if let Some(row) = s.rows.iter().find(|r| r.kind == LimitKind::Session) {
                    line.push_str(&format!(" · Session {:.0}%", row.percent.get()));
                }
                if let Some(row) = s.rows.iter().find(|r| r.kind == LimitKind::Weekly) {
                    line.push_str(&format!(" · Week {:.0}%", row.percent.get()));
                }
                tip.push_str(&format!("\n{line}"));
            }
            (None, Some(msg)) => {
                tip.push_str(&format!("\nCodex — {}", err_head(&msg)));
            }
            (None, None) => {}
        }
    }

    if let Some(detail) = app::error_details(Duration::from_secs(u64::from(
        POLL_SECS.load(Ordering::SeqCst),
    ))) {
        tip = detail.replace('\n', " ");
    }
    let icon = trayicon::build(&style, dark).unwrap_or_default();
    let mut nid = base_nid(owner);
    nid.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    nid.hIcon = icon;
    set_tip(&mut nid, &tip);
    let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    swap_prev_icon(icon);
}

unsafe fn swap_prev_icon(new_icon: HICON) {
    let old = PREV_ICON.swap(new_icon.0 as isize, Ordering::SeqCst);
    if old != 0 && old != new_icon.0 as isize {
        let _ = DestroyIcon(HICON(old as *mut _));
    }
}

unsafe fn remove_tray(owner: HWND) {
    let nid = base_nid(owner);
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
}

// ---------- menu ----------

unsafe fn show_menu(owner: HWND, x: i32, y: i32) {
    if demo::is_active() {
        return;
    }
    let Ok(menu) = CreatePopupMenu() else { return };
    let _ = AppendMenuW(menu, MF_STRING, IDM_REFRESH, w!("Refresh now"));
    let _ = AppendMenuW(menu, MF_STRING, IDM_SETTINGS, w!("Settings…"));
    let auto = util::autostart_enabled();
    let check = if auto { MF_CHECKED } else { MF_UNCHECKED };
    let _ = AppendMenuW(
        menu,
        MF_STRING | check,
        IDM_AUTOSTART,
        w!("Start with Windows"),
    );
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, IDM_QUIT, w!("Quit Claudometer"));

    let _ = SetForegroundWindow(owner);
    let cmd = TrackPopupMenuEx(
        menu,
        (TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY).0,
        x,
        y,
        owner,
        None,
    );
    let _ = DestroyMenu(menu);

    match cmd.0 as usize {
        IDM_REFRESH => spawn_fetch_all(RefreshTrigger::Manual),
        IDM_SETTINGS => open_settings(),
        IDM_AUTOSTART => util::set_autostart(!auto),
        IDM_QUIT => {
            let _ = DestroyWindow(owner);
        }
        _ => {}
    }
}

// ---------- fetch ----------

/// Refresh every enabled provider through the UI-owned state and poller.
fn spawn_fetch_all(trigger: RefreshTrigger) {
    spawn_fetch(ProviderId::Claude, trigger);
    if config::settings().codex_enabled {
        spawn_fetch(ProviderId::Codex, trigger);
    }
}

fn spawn_fetch(provider: ProviderId, trigger: RefreshTrigger) {
    app::refresh(
        provider,
        trigger,
        Duration::from_secs(u64::from(POLL_SECS.load(Ordering::SeqCst))),
    );
}
