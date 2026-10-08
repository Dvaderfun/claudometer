//! UI Automation providers for the two Direct2D windows. The providers contain
//! no app state: they derive names and bounds from the same view and geometry as
//! rendering, and dispatch actions back to the window's UI thread.

use windows::core::{implement, IUnknown, Interface, Result, BSTR, VARIANT};
use windows::Win32::Foundation::{E_NOTIMPL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::VT_I4;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetClientRect, GetWindowRect, PostMessageW, SendMessageW, WM_APP, WM_GETOBJECT,
};

use crate::{auth, config, demo, gfx, util, vibecode};

pub const WM_UIA_QUERY: u32 = WM_APP + 6;
pub const WM_UIA_FOCUS: u32 = WM_APP + 7;
pub const WM_UIA_INVOKE: u32 = WM_APP + 8;
pub const QUERY_FOCUS: usize = 0;
pub const QUERY_SCROLL: usize = 1;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    Flyout,
    Settings,
}

#[derive(Clone, Copy)]
enum Role {
    Button,
    Toggle(bool),
    Text,
}

struct Item {
    id: String,
    name: String,
    help: String,
    rect: D2D_RECT_F,
    role: Role,
    enabled: bool,
}

fn item(id: impl Into<String>, name: impl Into<String>, rect: D2D_RECT_F, role: Role) -> Item {
    Item {
        id: id.into(),
        name: name.into(),
        help: String::new(),
        rect,
        role,
        enabled: true,
    }
}

fn items(hwnd: HWND, kind: WindowKind) -> Vec<Item> {
    match kind {
        WindowKind::Flyout => flyout_items(hwnd),
        WindowKind::Settings => settings_items(hwnd),
    }
}

fn scrolled(mut rect: D2D_RECT_F, scroll: f32) -> D2D_RECT_F {
    rect.top -= scroll;
    rect.bottom -= scroll;
    rect
}

fn flyout_items(hwnd: HWND) -> Vec<Item> {
    let view = demo::active()
        .map(|state| state.view.clone())
        .unwrap_or_else(crate::current_view);
    let scroll = unsafe {
        f32::from_bits(SendMessageW(hwnd, WM_UIA_QUERY, WPARAM(QUERY_SCROLL), LPARAM(0)).0 as u32)
    };
    let (refresh, settings) = gfx::fly_btns();
    let mut wake = item(
        "KeepAwake",
        "Keep computer awake",
        scrolled(gfx::vibe_row(&view), scroll),
        Role::Toggle(!demo::is_active() && vibecode::is_on()),
    );
    wake.help = if demo::is_active() {
        "Demo mode makes no system changes".to_string()
    } else {
        vibecode::flyout_caption().to_string()
    };
    let mut result = vec![
        item(
            "Refresh",
            "Refresh usage",
            scrolled(refresh, scroll),
            Role::Button,
        ),
        item(
            "Settings",
            "Open settings",
            scrolled(settings, scroll),
            Role::Button,
        ),
        wake,
    ];
    for (index, (rect, name)) in gfx::accessible_rows(&view).into_iter().enumerate() {
        result.push(item(
            format!("Usage{index}"),
            name,
            scrolled(rect, scroll),
            Role::Text,
        ));
    }
    if let Some(detail) = crate::app::error_details(std::time::Duration::from_secs(u64::from(
        config::settings().poll_interval_seconds,
    ))) {
        for item in &mut result {
            item.help = detail.clone();
        }
    }
    result
}

fn settings_items(hwnd: HWND) -> Vec<Item> {
    let scroll = unsafe {
        f32::from_bits(SendMessageW(hwnd, WM_UIA_QUERY, WPARAM(QUERY_SCROLL), LPARAM(0)).0 as u32)
    };
    let rects = gfx::settings_rects(scroll);
    let mut result = Vec::with_capacity(gfx::N_CARDS);
    let synthetic = demo::active().map(|_| demo::settings_view(-1, -1));
    let settings = config::settings();
    let (account_help, account_button) = if let Some(view) = &synthetic {
        (view.account_caption.clone(), view.account_action)
    } else {
        let status = auth::snapshot();
        let caption = match status.connection {
            Some(auth::ClaudeConnection::Connected { plan }) if !plan.is_empty() => {
                format!("Connected, {plan}")
            }
            Some(auth::ClaudeConnection::Connected { .. }) => "Connected".to_string(),
            Some(auth::ClaudeConnection::Disconnected) => "Not connected".to_string(),
            Some(auth::ClaudeConnection::CliUnavailable) => "Claude Code is required".to_string(),
            Some(auth::ClaudeConnection::Problem(message)) => message,
            None => "Checking connection".to_string(),
        };
        (caption, if status.busy { "Cancel" } else { "Connect" })
    };
    let mut account = item(
        "ClaudeAccount",
        format!("Claude account, {account_button}"),
        rects[gfx::CARD_ACCOUNT],
        Role::Button,
    );
    account.help = account_help;
    result.push(account);

    let caps = if let Some(view) = &synthetic {
        view.caps_control
    } else {
        match util::caps_led_state() {
            util::CapsLedState::InstalledEnabled => gfx::CapsControl::Toggle(true),
            util::CapsLedState::InstalledDisabled => gfx::CapsControl::Toggle(false),
            util::CapsLedState::Error(_) => gfx::CapsControl::Retry,
            util::CapsLedState::Unavailable => gfx::CapsControl::Unavailable,
        }
    };
    let mut caps_item = item(
        "CapsStatusLight",
        "Caps Lock status light",
        rects[gfx::CARD_CAPS],
        match caps {
            gfx::CapsControl::Toggle(on) => Role::Toggle(on),
            _ => Role::Button,
        },
    );
    caps_item.enabled = !matches!(caps, gfx::CapsControl::Unavailable);
    result.push(caps_item);

    for (index, id, name, on) in [
        (
            gfx::CARD_AUTOSTART,
            "Autostart",
            "Start with Windows",
            synthetic
                .as_ref()
                .map_or_else(util::autostart_enabled, |view| view.autostart),
        ),
        (
            gfx::CARD_CODEX,
            "ShowCodex",
            "Show Codex usage",
            synthetic
                .as_ref()
                .map_or(settings.codex_enabled, |view| view.codex_on),
        ),
        (
            gfx::CARD_ALERTS,
            "UsageAlerts",
            "Alert at 75% usage",
            synthetic
                .as_ref()
                .map_or(settings.alerts_enabled, |view| view.alerts_on),
        ),
        (
            gfx::CARD_UPDATE_CHECKS,
            "UpdateChecks",
            gfx::UPDATE_CHECKS_LABEL,
            synthetic
                .as_ref()
                .map_or(settings.update_checks_enabled, |view| view.update_checks_on),
        ),
    ] {
        result.push(item(id, name, rects[index], Role::Toggle(on)));
    }

    let lid_status = vibecode::persistent_status();
    let lid_role = if let Some(view) = &synthetic {
        Role::Toggle(view.lid_on)
    } else if matches!(
        lid_status,
        vibecode::PersistentStatus::Disabled | vibecode::PersistentStatus::Applied
    ) {
        Role::Toggle(lid_status == vibecode::PersistentStatus::Applied)
    } else {
        Role::Button
    };
    let mut lid = item(
        "LidOverride",
        "Advanced, ignore lid close",
        rects[gfx::CARD_LID],
        lid_role,
    );
    lid.help = synthetic.as_ref().map_or_else(
        || "Restores on exit when enabled".to_string(),
        |view| view.lid_caption.clone(),
    );
    result.push(lid);

    let interval = synthetic
        .as_ref()
        .map_or(settings.poll_interval_seconds, |view| view.poll_secs);
    let mut refresh_interval = item(
        "RefreshInterval",
        format!(
            "Auto-refresh, every {}",
            match interval {
                30 => "30 seconds".to_string(),
                60 => "1 minute".to_string(),
                seconds => format!("{} minutes", seconds / 60),
            }
        ),
        rects[gfx::CARD_INTERVAL],
        Role::Button,
    );
    refresh_interval.help = "Press Left or Right to change the interval".to_string();
    result.push(refresh_interval);
    result.push(item(
        "RefreshNow",
        "Refresh usage now",
        rects[gfx::CARD_REFRESH],
        Role::Button,
    ));
    result.push(item(
        "About",
        format!("About Claudometer {}", env!("CARGO_PKG_VERSION")),
        rects[gfx::CARD_ABOUT],
        Role::Button,
    ));
    result.push(item(
        "Quit",
        "Quit Claudometer",
        rects[gfx::CARD_QUIT],
        Role::Button,
    ));
    result
}

unsafe fn bounds(hwnd: HWND, rect: D2D_RECT_F) -> Result<UiaRect> {
    let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
    let mut point = POINT {
        x: (rect.left * scale).round() as i32,
        y: (rect.top * scale).round() as i32,
    };
    ClientToScreen(hwnd, &mut point).ok()?;
    Ok(UiaRect {
        left: point.x as f64,
        top: point.y as f64,
        width: ((rect.right - rect.left) * scale) as f64,
        height: ((rect.bottom - rect.top) * scale) as f64,
    })
}

fn fragment(hwnd: isize, kind: WindowKind, index: usize) -> IRawElementProviderFragment {
    Element { hwnd, kind, index }.into()
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot
)]
struct Root {
    hwnd: isize,
    kind: WindowKind,
}

#[allow(non_snake_case)]
impl IRawElementProviderSimple_Impl for Root {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, _: UIA_PATTERN_ID) -> Result<IUnknown> {
        Err(E_NOTIMPL.into())
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let value = if property == UIA_NamePropertyId {
            VARIANT::from(BSTR::from(match self.kind {
                WindowKind::Flyout => "Claudometer usage",
                WindowKind::Settings => "Claudometer settings",
            }))
        } else if property == UIA_AutomationIdPropertyId {
            VARIANT::from(BSTR::from(match self.kind {
                WindowKind::Flyout => "UsageFlyout",
                WindowKind::Settings => "SettingsWindow",
            }))
        } else if property == UIA_ControlTypePropertyId {
            VARIANT::from(UIA_PaneControlTypeId.0)
        } else if property == UIA_IsControlElementPropertyId {
            VARIANT::from(true)
        } else {
            VARIANT::default()
        };
        Ok(value)
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        unsafe { UiaHostProviderFromHwnd(HWND(self.hwnd as *mut _)) }
    }
}

#[allow(non_snake_case)]
impl IRawElementProviderFragment_Impl for Root {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let count = items(HWND(self.hwnd as *mut _), self.kind).len();
        if count == 0 {
            return Err(E_NOTIMPL.into());
        }
        if direction == NavigateDirection_FirstChild {
            Ok(fragment(self.hwnd, self.kind, 0))
        } else if direction == NavigateDirection_LastChild {
            Ok(fragment(self.hwnd, self.kind, count - 1))
        } else {
            Err(E_NOTIMPL.into())
        }
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(HWND(self.hwnd as *mut _), &mut rect)? };
        Ok(UiaRect {
            left: rect.left as f64,
            top: rect.top as f64,
            width: (rect.right - rect.left) as f64,
            height: (rect.bottom - rect.top) as f64,
        })
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(Root {
            hwnd: self.hwnd,
            kind: self.kind,
        }
        .into())
    }
}

#[allow(non_snake_case)]
impl IRawElementProviderFragmentRoot_Impl for Root {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let hwnd = HWND(self.hwnd as *mut _);
        for (index, item) in items(hwnd, self.kind).iter().enumerate() {
            let rect = unsafe { bounds(hwnd, item.rect)? };
            if x >= rect.left
                && x <= rect.left + rect.width
                && y >= rect.top
                && y <= rect.top + rect.height
            {
                return Ok(fragment(self.hwnd, self.kind, index));
            }
        }
        Ok(Root {
            hwnd: self.hwnd,
            kind: self.kind,
        }
        .into())
    }

    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        let index = unsafe {
            SendMessageW(
                HWND(self.hwnd as *mut _),
                WM_UIA_QUERY,
                WPARAM(QUERY_FOCUS),
                LPARAM(0),
            )
            .0 as i32
        };
        if index >= 0 && (index as usize) < items(HWND(self.hwnd as *mut _), self.kind).len() {
            Ok(fragment(self.hwnd, self.kind, index as usize))
        } else {
            Err(E_NOTIMPL.into())
        }
    }
}

#[implement(IRawElementProviderSimple, IRawElementProviderFragment)]
struct Element {
    hwnd: isize,
    kind: WindowKind,
    index: usize,
}

impl Element {
    fn item(&self) -> Option<Item> {
        items(HWND(self.hwnd as *mut _), self.kind)
            .into_iter()
            .nth(self.index)
    }
}

#[allow(non_snake_case)]
impl IRawElementProviderSimple_Impl for Element {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
        let Some(item) = self.item() else {
            return Err(E_NOTIMPL.into());
        };
        let action = Action {
            hwnd: self.hwnd,
            kind: self.kind,
            index: self.index,
        };
        match item.role {
            Role::Button if pattern == UIA_InvokePatternId && item.enabled => {
                let provider: IInvokeProvider = action.into();
                provider.cast()
            }
            Role::Toggle(_) if pattern == UIA_TogglePatternId && item.enabled => {
                let provider: IToggleProvider = action.into();
                provider.cast()
            }
            _ => Err(E_NOTIMPL.into()),
        }
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let Some(item) = self.item() else {
            return Ok(VARIANT::default());
        };
        let value = if property == UIA_NamePropertyId {
            VARIANT::from(BSTR::from(item.name.as_str()))
        } else if property == UIA_AutomationIdPropertyId {
            VARIANT::from(BSTR::from(item.id.as_str()))
        } else if property == UIA_HelpTextPropertyId {
            VARIANT::from(BSTR::from(item.help.as_str()))
        } else if property == UIA_ControlTypePropertyId {
            VARIANT::from(match item.role {
                Role::Button => UIA_ButtonControlTypeId.0,
                Role::Toggle(_) => UIA_CheckBoxControlTypeId.0,
                Role::Text => UIA_TextControlTypeId.0,
            })
        } else if property == UIA_IsControlElementPropertyId
            || property == UIA_IsContentElementPropertyId
        {
            VARIANT::from(true)
        } else if property == UIA_IsKeyboardFocusablePropertyId {
            VARIANT::from(!matches!(item.role, Role::Text) && item.enabled)
        } else if property == UIA_IsEnabledPropertyId {
            VARIANT::from(item.enabled)
        } else if property == UIA_IsOffscreenPropertyId {
            let mut client = RECT::default();
            unsafe { GetClientRect(HWND(self.hwnd as *mut _), &mut client)? };
            let scale = unsafe { GetDpiForWindow(HWND(self.hwnd as *mut _)).max(96) as f32 / 96.0 };
            let viewport_height = (client.bottom - client.top) as f32 / scale;
            VARIANT::from(item.rect.bottom <= 0.0 || item.rect.top >= viewport_height)
        } else if property == UIA_HasKeyboardFocusPropertyId {
            let focus = unsafe {
                SendMessageW(
                    HWND(self.hwnd as *mut _),
                    WM_UIA_QUERY,
                    WPARAM(QUERY_FOCUS),
                    LPARAM(0),
                )
                .0 as i32
            };
            VARIANT::from(focus == self.index as i32)
        } else if property == UIA_ToggleToggleStatePropertyId {
            match item.role {
                Role::Toggle(on) => VARIANT::from(if on {
                    ToggleState_On.0
                } else {
                    ToggleState_Off.0
                }),
                _ => VARIANT::default(),
            }
        } else {
            VARIANT::default()
        };
        Ok(value)
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        Err(E_NOTIMPL.into())
    }
}

#[allow(non_snake_case)]
impl IRawElementProviderFragment_Impl for Element {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let count = items(HWND(self.hwnd as *mut _), self.kind).len();
        if direction == NavigateDirection_Parent {
            Ok(Root {
                hwnd: self.hwnd,
                kind: self.kind,
            }
            .into())
        } else if direction == NavigateDirection_NextSibling && self.index + 1 < count {
            Ok(fragment(self.hwnd, self.kind, self.index + 1))
        } else if direction == NavigateDirection_PreviousSibling && self.index > 0 {
            Ok(fragment(self.hwnd, self.kind, self.index - 1))
        } else {
            Err(E_NOTIMPL.into())
        }
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        unsafe {
            let array = SafeArrayCreateVector(VT_I4, 0, 2);
            if array.is_null() {
                return Err(windows::core::Error::from_win32());
            }
            let values = [UiaAppendRuntimeId as i32, self.index as i32 + 1];
            for (index, value) in values.iter().enumerate() {
                SafeArrayPutElement(array, &(index as i32), value as *const _ as *const _)?;
            }
            Ok(array)
        }
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        match self.item() {
            Some(item) => unsafe { bounds(HWND(self.hwnd as *mut _), item.rect) },
            None => Ok(UiaRect::default()),
        }
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        unsafe {
            PostMessageW(
                HWND(self.hwnd as *mut _),
                WM_UIA_FOCUS,
                WPARAM(self.index),
                LPARAM(0),
            )
        }
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(Root {
            hwnd: self.hwnd,
            kind: self.kind,
        }
        .into())
    }
}

#[implement(IInvokeProvider, IToggleProvider)]
struct Action {
    hwnd: isize,
    kind: WindowKind,
    index: usize,
}

#[allow(non_snake_case)]
impl IInvokeProvider_Impl for Action {
    fn Invoke(&self) -> Result<()> {
        unsafe {
            PostMessageW(
                HWND(self.hwnd as *mut _),
                WM_UIA_INVOKE,
                WPARAM(self.index),
                LPARAM(0),
            )
        }
    }
}

#[allow(non_snake_case)]
impl IToggleProvider_Impl for Action {
    fn Toggle(&self) -> Result<()> {
        self.Invoke()
    }

    fn ToggleState(&self) -> Result<ToggleState> {
        let item = items(HWND(self.hwnd as *mut _), self.kind)
            .into_iter()
            .nth(self.index);
        Ok(match item.map(|item| item.role) {
            Some(Role::Toggle(true)) => ToggleState_On,
            _ => ToggleState_Off,
        })
    }
}

// windows-rs 0.58 builds COM vtables for the generated *_Impl wrappers.
// Forward their calls to the provider implementations above.
macro_rules! delegate_simple {
    ($outer:ident) => {
        #[allow(non_snake_case)]
        impl IRawElementProviderSimple_Impl for $outer {
            fn ProviderOptions(&self) -> Result<ProviderOptions> {
                IRawElementProviderSimple_Impl::ProviderOptions(&self.this)
            }
            fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
                IRawElementProviderSimple_Impl::GetPatternProvider(&self.this, pattern)
            }
            fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> Result<VARIANT> {
                IRawElementProviderSimple_Impl::GetPropertyValue(&self.this, property)
            }
            fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
                IRawElementProviderSimple_Impl::HostRawElementProvider(&self.this)
            }
        }
    };
}

macro_rules! delegate_fragment {
    ($outer:ident) => {
        #[allow(non_snake_case)]
        impl IRawElementProviderFragment_Impl for $outer {
            fn Navigate(
                &self,
                direction: NavigateDirection,
            ) -> Result<IRawElementProviderFragment> {
                IRawElementProviderFragment_Impl::Navigate(&self.this, direction)
            }
            fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
                IRawElementProviderFragment_Impl::GetRuntimeId(&self.this)
            }
            fn BoundingRectangle(&self) -> Result<UiaRect> {
                IRawElementProviderFragment_Impl::BoundingRectangle(&self.this)
            }
            fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
                IRawElementProviderFragment_Impl::GetEmbeddedFragmentRoots(&self.this)
            }
            fn SetFocus(&self) -> Result<()> {
                IRawElementProviderFragment_Impl::SetFocus(&self.this)
            }
            fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
                IRawElementProviderFragment_Impl::FragmentRoot(&self.this)
            }
        }
    };
}

delegate_simple!(Root_Impl);
delegate_fragment!(Root_Impl);
delegate_simple!(Element_Impl);
delegate_fragment!(Element_Impl);

#[allow(non_snake_case)]
impl IRawElementProviderFragmentRoot_Impl for Root_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        IRawElementProviderFragmentRoot_Impl::ElementProviderFromPoint(&self.this, x, y)
    }
    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        IRawElementProviderFragmentRoot_Impl::GetFocus(&self.this)
    }
}

#[allow(non_snake_case)]
impl IInvokeProvider_Impl for Action_Impl {
    fn Invoke(&self) -> Result<()> {
        IInvokeProvider_Impl::Invoke(&self.this)
    }
}

#[allow(non_snake_case)]
impl IToggleProvider_Impl for Action_Impl {
    fn Toggle(&self) -> Result<()> {
        IToggleProvider_Impl::Toggle(&self.this)
    }
    fn ToggleState(&self) -> Result<ToggleState> {
        IToggleProvider_Impl::ToggleState(&self.this)
    }
}

pub unsafe fn get_object(hwnd: HWND, wparam: WPARAM, lparam: LPARAM, kind: WindowKind) -> LRESULT {
    if lparam.0 as i32 != UiaRootObjectId {
        return DefWindowProcW(hwnd, WM_GETOBJECT, wparam, lparam);
    }
    let provider: IRawElementProviderSimple = Root {
        hwnd: hwnd.0 as isize,
        kind,
    }
    .into();
    UiaReturnRawElementProvider(hwnd, wparam, lparam, &provider)
}

pub fn focus_changed(hwnd: HWND, kind: WindowKind, index: usize) {
    unsafe {
        if !UiaClientsAreListening().as_bool() {
            return;
        }
        let provider: IRawElementProviderSimple = Element {
            hwnd: hwnd.0 as isize,
            kind,
            index,
        }
        .into();
        let _ = UiaRaiseAutomationEvent(&provider, UIA_AutomationFocusChangedEventId);
    }
}
