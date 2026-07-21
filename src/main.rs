//! Claudometer — Claude usage limits in the Windows 11 tray.
//! Native Win32: tray ring icon + acrylic DirectComposition flyout + Mica settings.

#![windows_subsystem = "windows"]
#![allow(clippy::missing_safety_doc)]

mod alerts;
mod api;
mod codex;
mod gfx;
mod trayicon;
mod updater;
mod util;

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE, VK_LEFT, VK_RETURN,
    VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 1;
const WM_DATA_READY: u32 = WM_APP + 2;
/// toast clicked (posted from a WinRT threadpool thread by alerts.rs)
pub(crate) const WM_TOAST_ACTIVATED: u32 = WM_APP + 3;
/// updater state changed (wparam 0) / handover ready, quit now (wparam 1)
pub(crate) const WM_UPDATE: u32 = WM_APP + 4;

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Provider {
    Claude = 0,
    Codex = 1,
}

/// Per-provider fetch state; both providers share the resilience rules
/// (stale data beats errors, Retry-After honored exactly, 3 s debounce).
struct Slot {
    state: Mutex<Option<api::FetchOutcome>>,
    last_good: Mutex<Option<api::UsageSnapshot>>,
    last_fetch: Mutex<Option<Instant>>,
    /// Retry-After honored exactly: no requests before this instant
    cooldown_until: Mutex<Option<Instant>>,
    fetching: AtomicBool,
}

impl Slot {
    const fn new() -> Self {
        Self {
            state: Mutex::new(None),
            last_good: Mutex::new(None),
            last_fetch: Mutex::new(None),
            cooldown_until: Mutex::new(None),
            fetching: AtomicBool::new(false),
        }
    }
}

static SLOTS: [Slot; 2] = [Slot::new(), Slot::new()];

fn slot(p: Provider) -> &'static Slot {
    &SLOTS[p as usize]
}

fn any_fetching() -> bool {
    SLOTS.iter().any(|s| s.fetching.load(Ordering::SeqCst))
}

/// Codex section is live: toggle on AND a ChatGPT-login auth file on disk.
fn codex_active() -> bool {
    util::show_codex() && codex::available()
}

/// Snapshot to display for a provider (fresh, or stale on error) plus the
/// current error message when the last fetch failed.
fn effective(p: Provider) -> (Option<api::UsageSnapshot>, Option<String>) {
    let s = slot(p);
    let state = s.state.lock().unwrap();
    match &*state {
        Some(api::FetchOutcome::Ok(snap)) => (Some(snap.clone()), None),
        Some(api::FetchOutcome::Err { msg, .. }) => {
            (s.last_good.lock().unwrap().clone(), Some(msg.clone()))
        }
        None => (s.last_good.lock().unwrap().clone(), None),
    }
}

struct Ui {
    fly: Option<gfx::Surface>,
    set: Option<gfx::Surface>,
    fly_hover: gfx::FlyHover,
    set_hover: i32,
    fly_focus: i32, // keyboard focus: -1 none, 0 refresh, 1 gear
    set_focus: i32, // keyboard focus card index, -1 none
    fly_tracking: bool,
    set_tracking: bool,
}

thread_local! {
    static UI: RefCell<Ui> = const {
        RefCell::new(Ui {
            fly: None,
            set: None,
            fly_hover: gfx::FlyHover::None,
            set_hover: -1,
            fly_focus: -1,
            set_focus: -1,
            fly_tracking: false,
            set_tracking: false,
        })
    };
}

fn main() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);

        // hidden verification hook: fire a fake 75% toast and exit
        if std::env::args().any(|a| a == "--test-alert") {
            alerts::init();
            alerts::show_test();
            std::thread::sleep(std::time::Duration::from_secs(2));
            return Ok(());
        }

        let mutex = CreateMutexW(None, true, w!("Local\\Claudometer.SingleInstance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // normal double-launch: exit silently. `--swap-wait` = we're the
            // fresh exe of an update handover — the old instance is exiting;
            // its mutex signals abandoned once its process dies.
            if !std::env::args().any(|a| a == "--swap-wait") {
                return Ok(());
            }
            let w = WaitForSingleObject(mutex, 15_000);
            if w != WAIT_OBJECT_0 && w != WAIT_ABANDONED {
                return Ok(());
            }
        }
        std::thread::spawn(updater::cleanup_old);

        util::enable_dark_context_menus();
        alerts::init();

        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        TASKBAR_MSG.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::SeqCst);

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
        let main = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            cls,
            w!("Claudometer"),
            WS_POPUP,
            0, 0, 0, 0,
            None, None, hinst, None,
        )?;
        MAIN_HWND.store(main.0 as isize, Ordering::SeqCst);

        // flyout window
        let fcls = w!("Claudometer.Flyout");
        let fwc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(flyout_wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: fcls,
            ..Default::default()
        };
        RegisterClassExW(&fwc);
        let flyout = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST,
            fcls,
            w!("Claude usage"),
            WS_POPUP,
            0, 0, 10, 10,
            None, None, hinst, None,
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
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: app_icon,
            lpszClassName: scls,
            ..Default::default()
        };
        RegisterClassExW(&swc);

        add_tray_icon(main);
        POLL_SECS.store(util::load_poll_secs(), Ordering::SeqCst);
        SetTimer(main, TIMER_POLL, POLL_SECS.load(Ordering::SeqCst) * 1000, None);
        // TIMER_TICK runs only while the flyout is visible (started in show_flyout)
        spawn_fetch_all();
        updater::maybe_check();

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
                update_tray(hwnd);
                run_alert_checks();
                if IsWindowVisible(flyout_hwnd()).as_bool() {
                    show_flyout(ANCHOR_X.load(Ordering::SeqCst), ANCHOR_Y.load(Ordering::SeqCst));
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
                spawn_fetch_all();
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
                if setting_change_is_theme(lparam) {
                    update_tray(hwnd);
                    apply_flyout_theme(flyout_hwnd());
                    if IsWindowVisible(flyout_hwnd()).as_bool() {
                        show_flyout(ANCHOR_X.load(Ordering::SeqCst), ANCHOR_Y.load(Ordering::SeqCst));
                    }
                    let sh = settings_hwnd();
                    if !sh.is_invalid() {
                        apply_settings_theme(sh);
                        if IsWindowVisible(sh).as_bool() {
                            render_settings(sh);
                        }
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DESTROY => {
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
            WM_ACTIVATE => {
                if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                    hide_flyout();
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let vk = wparam.0 as u16;
                if vk == VK_ESCAPE.0 {
                    hide_flyout();
                } else if vk == VK_TAB.0 {
                    UI.with(|ui| {
                        let mut ui = ui.borrow_mut();
                        ui.fly_focus = if ui.fly_focus == 0 { 1 } else { 0 };
                    });
                    render_flyout_current();
                } else if vk == VK_RETURN.0 || vk == VK_SPACE.0 {
                    match UI.with(|ui| ui.borrow().fly_focus) {
                        0 => {
                            spawn_fetch_all();
                            render_flyout_current();
                        }
                        1 => {
                            hide_flyout();
                            open_settings();
                        }
                        _ => {}
                    }
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
                match fly_hit(x, y) {
                    gfx::FlyHover::Refresh => {
                        spawn_fetch_all();
                        render_flyout_current(); // spinner starts immediately
                    }
                    gfx::FlyHover::Gear => {
                        hide_flyout();
                        open_settings();
                    }
                    gfx::FlyHover::None => {}
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

extern "system" fn settings_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
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
                }
                if hit == gfx::CARD_INTERVAL as i32 {
                    // pill click selects the interval directly
                    let cards = gfx::settings_rects();
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
            WM_KEYDOWN => {
                let vk = wparam.0 as u16;
                if vk == VK_ESCAPE.0 {
                    hide_settings(hwnd);
                } else if vk == VK_TAB.0 {
                    let back = GetKeyState(VK_SHIFT.0 as i32) < 0;
                    UI.with(|ui| {
                        let mut ui = ui.borrow_mut();
                        let n = gfx::N_CARDS as i32;
                        ui.set_focus = if ui.set_focus < 0 {
                            if back { n - 1 } else { 0 }
                        } else if back {
                            (ui.set_focus - 1 + n) % n
                        } else {
                            (ui.set_focus + 1) % n
                        };
                    });
                    render_settings(hwnd);
                } else if vk == VK_LEFT.0 || vk == VK_RIGHT.0 {
                    if UI.with(|ui| ui.borrow().set_focus) == gfx::CARD_INTERVAL as i32 {
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
                hide_settings(hwnd);
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
                render_settings(hwnd);
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
    s == "ImmersiveColorSet"
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
    POLL_SECS.store(secs, Ordering::SeqCst);
    util::save_poll_secs(secs);
    let mh = HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _);
    let _ = KillTimer(mh, TIMER_POLL);
    SetTimer(mh, TIMER_POLL, secs * 1000, None);
}

unsafe fn step_interval(dir: i32) {
    let cur = POLL_SECS.load(Ordering::SeqCst);
    let idx = gfx::INTERVALS.iter().position(|(s, _)| *s == cur).unwrap_or(1) as i32;
    let next = (idx + dir).clamp(0, gfx::INTERVALS.len() as i32 - 1) as usize;
    apply_interval(gfx::INTERVALS[next].0);
}

unsafe fn activate_settings_card(hwnd: HWND, i: usize) {
    match i {
        0 => util::set_caps_led_enabled(!util::caps_led_enabled()),
        1 => util::set_autostart(!util::autostart_enabled()),
        2 => {
            let on = !util::show_codex();
            util::set_show_codex(on);
            if on {
                spawn_fetch(Provider::Codex);
            }
            update_tray(HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _));
        }
        3 => util::set_alerts_enabled(!util::alerts_enabled()),
        4 => {
            // keyboard activate on the interval card: cycle to the next option
            let cur = POLL_SECS.load(Ordering::SeqCst);
            let idx = gfx::INTERVALS.iter().position(|(s, _)| *s == cur).unwrap_or(1);
            apply_interval(gfx::INTERVALS[(idx + 1) % gfx::INTERVALS.len()].0);
        }
        5 => spawn_fetch_all(),
        6 => match updater::status() {
            updater::Status::Available(_) => updater::install(),
            updater::Status::Installing => {}
            updater::Status::Failed(_, page) => {
                updater::open_url(page.as_deref().unwrap_or(updater::REPO_URL));
            }
            updater::Status::UpToDate => updater::open_url(updater::REPO_URL),
        },
        7 => {
            let _ = DestroyWindow(HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _));
            return;
        }
        _ => {}
    }
    render_settings(hwnd);
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
    let (refresh, gear) = gfx::fly_btns();
    if contains(&refresh, x, y) {
        gfx::FlyHover::Refresh
    } else if contains(&gear, x, y) {
        gfx::FlyHover::Gear
    } else {
        gfx::FlyHover::None
    }
}

fn settings_hit(x: f32, y: f32) -> i32 {
    for (i, card) in gfx::settings_rects().iter().enumerate() {
        if contains(card, x, y) {
            return i as i32;
        }
    }
    -1
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
    apply_flyout_theme(h);
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
    util::apply_acrylic(h, dark);
}

/// Last good snapshot survives transient errors (429, network blips):
/// the flyout and tray keep showing stale data; the whole-flyout error view
/// only appears when nothing was ever fetched from any provider. Per-provider
/// failures degrade to a dim note line inside that provider's section.
fn current_view() -> gfx::View {
    let (c_snap, c_err) = effective(Provider::Claude);
    let codex_on = codex_active();

    // Claude-only path — identical to the single-provider behavior
    if !codex_on {
        return match (c_snap, c_err) {
            (None, None) => gfx::View::Loading,
            (None, Some(msg)) => gfx::View::Error(msg),
            (Some(s), err) => gfx::View::Data(gfx::FlyoutData {
                fetched_unix: Some(s.fetched_unix),
                note: err.as_deref().map(err_head),
                sections: vec![section("Claude", s)],
            }),
        };
    }

    let (x_snap, x_err) = effective(Provider::Codex);
    if c_snap.is_none() && c_err.is_none() && x_snap.is_none() && x_err.is_none() {
        return gfx::View::Loading;
    }

    let mut sections = Vec::new();
    let mut notes = Vec::new();
    let mut fetched: Option<i64> = None;
    for (title, snap, err) in [("Claude", c_snap, c_err), ("Codex", x_snap, x_err)] {
        match (snap, err) {
            (Some(s), err) => {
                fetched = fetched.max(Some(s.fetched_unix));
                if err.is_some() {
                    notes.push(format!("{title}: {}", err.as_deref().map(err_head).unwrap_or_default()));
                }
                sections.push(section(title, s));
            }
            (None, Some(msg)) => sections.push(gfx::Section {
                title,
                plan: String::new(),
                body: gfx::SectionBody::Note(msg.lines().next().unwrap_or("Can't load usage").to_string()),
            }),
            (None, None) => sections.push(gfx::Section {
                title,
                plan: String::new(),
                body: gfx::SectionBody::Note("Loading…".to_string()),
            }),
        }
    }
    gfx::View::Data(gfx::FlyoutData {
        sections,
        fetched_unix: fetched,
        note: if notes.is_empty() { None } else { Some(notes.join(" · ")) },
    })
}

fn section(title: &'static str, s: api::UsageSnapshot) -> gfx::Section {
    gfx::Section {
        title,
        plan: s.plan,
        body: gfx::SectionBody::Rows(s.rows),
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
    ANCHOR_X.store(cx, Ordering::SeqCst);
    ANCHOR_Y.store(cy, Ordering::SeqCst);
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.fly_hover = gfx::FlyHover::None;
        ui.fly_focus = -1;
    });

    let stale = |p: Provider| {
        slot(p)
            .last_fetch
            .lock()
            .unwrap()
            .map(|t| t.elapsed().as_secs() > 15)
            .unwrap_or(true)
    };
    if stale(Provider::Claude) {
        spawn_fetch(Provider::Claude);
    }
    if codex_active() && stale(Provider::Codex) {
        spawn_fetch(Provider::Codex);
    }

    let fh = flyout_hwnd();
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
    let h_px = (gfx::flyout_height(&view) * scale).round() as i32;

    let margin = (12.0 * scale).round() as i32;
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
    SetTimer(HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _), TIMER_TICK, 30_000, None);
}

unsafe fn render_flyout(fh: HWND, view: &gfx::View, w_px: u32, h_px: u32, dpi: f32) {
    let dark = util::is_dark_theme();
    let accent = util::accent_rgb();
    let fetching = any_fetching();
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        if ui.fly.is_none() {
            ui.fly = gfx::Surface::new(fh).ok();
        }
        let hover = ui.fly_hover;
        let focus = ui.fly_focus;
        if let Some(fx) = ui.fly.as_mut() {
            let _ = fx.render_flyout(
                w_px, h_px, dpi, view, dark, accent, hover, focus, fetching,
                updater::has_update(),
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
    UI.with(|ui| ui.borrow_mut().set_focus = -1);
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
    let client_h = (gfx::settings_height() * scale).round() as i32;
    let mut rc = RECT { left: 0, top: 0, right: client_w, bottom: client_h };
    let _ = AdjustWindowRectExForDpi(&mut rc, style, false, WS_EX_NOREDIRECTIONBITMAP, dpix);
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;

    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = GetMonitorInfoW(hmon, &mut mi);
    let x = mi.rcWork.left + (mi.rcWork.right - mi.rcWork.left - w) / 2;
    let y = mi.rcWork.top + (mi.rcWork.bottom - mi.rcWork.top - h) / 2;

    let Ok(hwnd) = CreateWindowExW(
        WS_EX_NOREDIRECTIONBITMAP,
        w!("Claudometer.Settings"),
        w!("Claudometer"),
        style,
        x, y, w, h,
        None, None, hinst, None,
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
    // Mica over the whole window ("sheet of glass" + main-window backdrop)
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    let _ = DwmExtendFrameIntoClientArea(h, &margins);
    let backdrop = DWMSBT_MAINWINDOW;
    let _ = DwmSetWindowAttribute(
        h,
        DWMWA_SYSTEMBACKDROP_TYPE,
        &backdrop as *const _ as *const _,
        std::mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
    );
    let dark = BOOL(if util::is_dark_theme() { 1 } else { 0 });
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
    let dark = util::is_dark_theme();
    let accent = util::accent_rgb();
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        if ui.set.is_none() {
            ui.set = gfx::Surface::new(hwnd).ok();
        }
        let (about, about_btn) = match updater::status() {
            updater::Status::UpToDate => {
                (concat!("Claudometer ", env!("CARGO_PKG_VERSION")).to_string(), "GitHub")
            }
            updater::Status::Available(r) => (format!("Update {} available", r.tag), "Install"),
            updater::Status::Installing => ("Installing update…".to_string(), "…"),
            updater::Status::Failed(msg, _) => (format!("Update failed — {msg}"), "GitHub"),
        };
        let st = gfx::SettingsView {
            caps_on: util::caps_led_enabled(),
            autostart: util::autostart_enabled(),
            codex_on: util::show_codex(),
            alerts_on: util::alerts_enabled(),
            about,
            about_btn,
            update_ready: updater::has_update(),
            poll_secs: POLL_SECS.load(Ordering::SeqCst),
            hover: ui.set_hover,
            focus: ui.set_focus,
        };
        if let Some(sx) = ui.set.as_mut() {
            let _ = sx.render_settings(
                (rc.right - rc.left) as u32,
                (rc.bottom - rc.top) as u32,
                dpi,
                &st,
                dark,
                accent,
            );
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

unsafe fn update_tray(owner: HWND) {
    let dark = util::is_dark_theme();
    let accent = util::accent_rgb();

    // ring + first tooltip line stay Claude's (stale data beats an error icon)
    let (c_snap, c_err) = effective(Provider::Claude);
    let (style, mut tip) = match (&c_snap, &c_err) {
        (Some(s), _) => {
            let session = s.rows.iter().find(|r| r.kind == "session");
            let weekly = s.rows.iter().find(|r| r.kind == "weekly_all");
            let (frac, rgb, mut tip) = match session {
                Some(row) => (
                    (row.percent / 100.0) as f32,
                    util::severity_rgb(&row.severity, row.percent, accent),
                    format!("Claude · Session {:.0}%", row.percent),
                ),
                None => (0.0, accent, "Claude".to_string()),
            };
            if let Some(wk) = weekly {
                tip.push_str(&format!(" · Week {:.0}%", wk.percent));
            }
            if let Some(row) = session {
                if !row.reset_text.is_empty() {
                    tip.push_str(&format!(" · {}", row.reset_text));
                }
            }
            (trayicon::Style::Ring { frac, rgb }, tip)
        }
        (None, Some(msg)) => (
            trayicon::Style::Alert,
            format!("Claude — {}", msg.replace('\n', " ")),
        ),
        (None, None) => (trayicon::Style::Loading, "Claude — loading usage…".to_string()),
    };

    if codex_active() {
        match effective(Provider::Codex) {
            (Some(s), _) => {
                let mut line = "Codex".to_string();
                if let Some(row) = s.rows.iter().find(|r| r.kind == "session") {
                    line.push_str(&format!(" · Session {:.0}%", row.percent));
                }
                if let Some(row) = s.rows.iter().find(|r| r.kind == "weekly_all") {
                    line.push_str(&format!(" · Week {:.0}%", row.percent));
                }
                tip.push_str(&format!("\n{line}"));
            }
            (None, Some(msg)) => {
                tip.push_str(&format!("\nCodex — {}", err_head(&msg)));
            }
            (None, None) => {}
        }
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
    let Ok(menu) = CreatePopupMenu() else { return };
    let _ = AppendMenuW(menu, MF_STRING, IDM_REFRESH, w!("Refresh now"));
    let _ = AppendMenuW(menu, MF_STRING, IDM_SETTINGS, w!("Settings…"));
    let auto = util::autostart_enabled();
    let check = if auto { MF_CHECKED } else { MF_UNCHECKED };
    let _ = AppendMenuW(menu, MF_STRING | check, IDM_AUTOSTART, w!("Start with Windows"));
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
        IDM_REFRESH => spawn_fetch_all(),
        IDM_SETTINGS => open_settings(),
        IDM_AUTOSTART => util::set_autostart(!auto),
        IDM_QUIT => {
            let _ = DestroyWindow(owner);
        }
        _ => {}
    }
}

// ---------- alerts ----------

/// After every fetch: hand *fresh* snapshots to the alert engine. Stale
/// (error-preserved) data must never alert — only a just-succeeded fetch.
fn run_alert_checks() {
    let fresh = |p: Provider| match &*slot(p).state.lock().unwrap() {
        Some(api::FetchOutcome::Ok(s)) => Some(s.clone()),
        _ => None,
    };
    if let Some(s) = fresh(Provider::Claude) {
        alerts::check("Claude", &s);
    }
    if codex_active() {
        if let Some(s) = fresh(Provider::Codex) {
            alerts::check("Codex", &s);
        }
    }
}

// ---------- fetch ----------

/// Refresh every live provider (Claude always, Codex when active).
fn spawn_fetch_all() {
    spawn_fetch(Provider::Claude);
    if codex_active() {
        spawn_fetch(Provider::Codex);
    }
}

fn spawn_fetch(p: Provider) {
    let s = slot(p);
    // Retry-After from a 429 is honored exactly — no requests inside the window
    if let Some(until) = *s.cooldown_until.lock().unwrap() {
        if Instant::now() < until {
            return;
        }
    }
    // debounce: manual refresh spam turns into API 429s
    if let Some(t) = *s.last_fetch.lock().unwrap() {
        if t.elapsed().as_secs() < 3 {
            return;
        }
    }
    if s.fetching.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let out = match p {
            Provider::Claude => api::fetch(),
            Provider::Codex => codex::fetch(),
        };
        let s = slot(p);
        match &out {
            api::FetchOutcome::Ok(snap) => {
                *s.last_good.lock().unwrap() = Some(snap.clone());
                *s.cooldown_until.lock().unwrap() = None;
            }
            api::FetchOutcome::Err { retry_after, .. } => {
                if let Some(secs) = retry_after {
                    let capped = (*secs).min(300);
                    *s.cooldown_until.lock().unwrap() =
                        Some(Instant::now() + std::time::Duration::from_secs(capped));
                }
            }
        }
        *s.state.lock().unwrap() = Some(out);
        *s.last_fetch.lock().unwrap() = Some(Instant::now());
        s.fetching.store(false, Ordering::SeqCst);
        let h = MAIN_HWND.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(HWND(h as *mut _), WM_DATA_READY, WPARAM(0), LPARAM(0));
            }
        }
    });
}
