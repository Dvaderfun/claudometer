//! "Vibecode mode" — keep the machine working with the lid shut.
//!
//! Two independent, fully reversible switches:
//!
//! * **Wake lock** — `SetThreadExecutionState(ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)`
//!   held by the UI thread. Process-scoped: it dies with the thread, so there
//!   is nothing to clean up system-side. Must be called from the UI thread —
//!   the flag is per-thread, not per-process.
//! * **Lid-close action** — the active power scheme's "Lid close action" is
//!   forced to *Do nothing* on AC and DC. This one outlives the process, so
//!   the previous indices are written to settings.json **before** the first
//!   override and restored verbatim on disable. A crash mid-mode leaves them
//!   on disk; the next launch re-arms without overwriting them, and turning
//!   the mode off still restores the user's original values.
//!
//! Machines with no lid setting (desktops) simply get the wake lock.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::GUID;
use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
use windows::Win32::System::Power::*;
use windows::Win32::System::Registry::HKEY;

use crate::util;

/// Buttons-and-lid subgroup + "Lid close action" setting. windows 0.58 exports
/// neither GUID under the features we enable — powercfg's own values.
const SUB_BUTTONS: GUID = GUID::from_u128(0x4f971e89_eebd_4455_a8de_9e59040e7347);
const LID_CLOSE_ACTION: GUID = GUID::from_u128(0x5ca83367_6e45_459f_a27b_476b1d01c936);
/// Lid-close action index 0 = "Do nothing".
const LID_DO_NOTHING: u32 = 0;

static ON: AtomicBool = AtomicBool::new(false);

pub fn is_on() -> bool {
    ON.load(Ordering::SeqCst)
}

/// Re-arm at startup when the mode was left on — it's a persisted preference,
/// and re-arming is also what restores the override after a crash.
pub fn init() {
    if util::vibecode_enabled() {
        arm();
    }
}

/// User toggled the flyout row. Call on the UI thread.
pub fn set(on: bool) {
    if on {
        arm();
    } else {
        disarm();
    }
    util::set_vibecode_enabled(on);
}

/// Quit path: undo the system-side override but keep the preference (and the
/// saved lid values) on disk, so the next launch re-arms and can still restore.
pub fn restore_for_exit() {
    if is_on() {
        restore_system();
    }
}

fn arm() {
    // Save the originals exactly once — never let our own "Do nothing" value
    // overwrite a saved pair (that would lose the user's real setting).
    let saved = util::vibecode_saved_lid().or_else(|| {
        let cur = read_lid();
        util::save_vibecode_lid(cur);
        cur
    });
    if saved.is_some() {
        write_lid((LID_DO_NOTHING, LID_DO_NOTHING));
    }
    wake_lock(true);
    ON.store(true, Ordering::SeqCst);
}

fn disarm() {
    restore_system();
    util::save_vibecode_lid(None); // nothing left to undo
}

fn restore_system() {
    if let Some(prev) = util::vibecode_saved_lid() {
        write_lid(prev);
    }
    wake_lock(false);
    ON.store(false, Ordering::SeqCst);
}

fn wake_lock(on: bool) {
    unsafe {
        let flags = if on {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
        } else {
            ES_CONTINUOUS
        };
        SetThreadExecutionState(flags);
    }
}

/// Run `f` with the active power scheme's GUID (a LocalAlloc'd copy we own).
fn with_active_scheme<T>(f: impl FnOnce(&GUID) -> T) -> Option<T> {
    unsafe {
        let mut p: *mut GUID = std::ptr::null_mut();
        if PowerGetActiveScheme(HKEY::default(), &mut p) != ERROR_SUCCESS || p.is_null() {
            return None;
        }
        let out = f(&*p);
        let _ = LocalFree(HLOCAL(p as *mut _));
        Some(out)
    }
}

/// Current (AC, DC) lid-close action indices, or None when the machine has no
/// lid setting at all.
fn read_lid() -> Option<(u32, u32)> {
    with_active_scheme(|scheme| unsafe {
        let mut ac = 0u32;
        let mut dc = 0u32;
        let ok_ac = PowerReadACValueIndex(
            HKEY::default(),
            Some(scheme),
            Some(&SUB_BUTTONS),
            Some(&LID_CLOSE_ACTION),
            &mut ac,
        ) == ERROR_SUCCESS;
        let ok_dc = PowerReadDCValueIndex(
            HKEY::default(),
            Some(scheme),
            Some(&SUB_BUTTONS),
            Some(&LID_CLOSE_ACTION),
            &mut dc,
        ) == 0;
        (ok_ac && ok_dc).then_some((ac, dc))
    })
    .flatten()
}

fn write_lid((ac, dc): (u32, u32)) {
    with_active_scheme(|scheme| unsafe {
        let _ = PowerWriteACValueIndex(
            HKEY::default(),
            scheme,
            Some(&SUB_BUTTONS),
            Some(&LID_CLOSE_ACTION),
            ac,
        );
        let _ = PowerWriteDCValueIndex(
            HKEY::default(),
            scheme,
            Some(&SUB_BUTTONS),
            Some(&LID_CLOSE_ACTION),
            dc,
        );
        // The writes land in the scheme's stored copy — re-activating the same
        // scheme is what pushes them into the running power policy.
        let _ = PowerSetActiveScheme(HKEY::default(), Some(scheme));
    });
}
