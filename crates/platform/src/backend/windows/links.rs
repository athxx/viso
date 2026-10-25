//! The URLs the app is opened with. Windows starts the program with the URL
//! as its argument, so each start first looks for an instance of the app
//! already running (a message-only window named for it) and hands that one
//! its URLs with `WM_COPYDATA`, then ends; otherwise it becomes the instance
//! others hand theirs to. An app not installed from an MSIX registers itself
//! as the handler of its schemes under the user's classes, as nothing
//! installed it.

use std::cell::RefCell;
use std::rc::Weak;

use ::windows::Win32::Foundation::{
    APPMODEL_ERROR_NO_PACKAGE, ERROR_SUCCESS, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use ::windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
use ::windows::Win32::System::DataExchange::COPYDATASTRUCT;
use ::windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegGetValueW, RegSetKeyValueW,
};
use ::windows::Win32::System::Threading::GetCurrentThreadId;
use ::windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, FindWindowExW,
    GetWindowThreadProcessId, HWND_MESSAGE, IsIconic, PostThreadMessageW, RegisterClassExW,
    SMTO_ABORTIFHUNG, SW_RESTORE, SendMessageTimeoutW, SetForegroundWindow, ShowWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_COPYDATA, WNDCLASSEXW,
};
use ::windows::core::{PCWSTR, w};

use super::super::links::{declared_schemes, instance_name, is_link, launch_urls};
use super::super::win32_translate::wide;
use super::{PumpQueue, WM_WAKE};
use crate::event::RawEvent;

/// The class of the window an instance receives other starts' URLs on.
const CLASS: PCWSTR = w!("VisoLinks");
/// What marks a `WM_COPYDATA` as a URL from another start of the app.
const URL_DATA: usize = 0x5649_534c;
/// How long a start waits for the running instance to take a URL.
const HAND_OVER_MS: u32 = 5000;

thread_local! {
    /// The pump's queue, for the window that receives other starts' URLs.
    static QUEUE: RefCell<Weak<RefCell<PumpQueue>>> = const { RefCell::new(Weak::new()) };
}

/// The URLs this start opens, once launched. With an instance of the app
/// already running, hands them to it and ends the process instead. Nothing
/// for an app that declares no scheme.
pub(super) fn claim(queue: &super::Shared, hinstance: HINSTANCE) -> Vec<String> {
    if declared_schemes().next().is_none() {
        return Vec::new();
    }
    let urls = launch_urls();
    let name = wide(&instance_name());
    if !urls.is_empty() && hand_over(&name, &urls) {
        std::process::exit(0);
    }
    QUEUE.with(|q| *q.borrow_mut() = std::rc::Rc::downgrade(queue));
    listen(&name, hinstance);
    if !packaged() {
        register_schemes();
    }
    urls
}

/// Hands `urls` to the running instance named `name`; `false` when there is
/// none, or it took not all of them.
fn hand_over(name: &[u16], urls: &[String]) -> bool {
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let Ok(hwnd) =
        (unsafe { FindWindowExW(Some(HWND_MESSAGE), None, CLASS, PCWSTR(name.as_ptr())) })
    else {
        return false;
    };
    let mut pid = 0u32;
    // SAFETY: `hwnd` is a window handle (a stale one fails the calls) and
    // `pid` a live out-pointer. Letting the instance take the foreground is
    // what lets it bring its window up for the URL.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
    }
    urls.iter().all(|url| {
        let text: Vec<u16> = url.encode_utf16().collect();
        let data = COPYDATASTRUCT {
            dwData: URL_DATA,
            cbData: (text.len() * 2) as u32,
            lpData: text.as_ptr() as *mut _,
        };
        let mut answer = 0usize;
        // SAFETY: `data` and the text it points at outlive the call, which
        // copies them into the receiving process before it returns.
        let sent = unsafe {
            SendMessageTimeoutW(
                hwnd,
                WM_COPYDATA,
                WPARAM(0),
                LPARAM(&raw const data as isize),
                SMTO_ABORTIFHUNG,
                HAND_OVER_MS,
                Some(&mut answer),
            )
        };
        sent.0 != 0 && answer == 1
    })
}

/// Opens the message-only window named `name` the next starts hand their
/// URLs to.
fn listen(name: &[u16], hinstance: HINSTANCE) {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(links_proc),
        hInstance: hinstance,
        lpszClassName: CLASS,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialized and its strings are static; the
    // name outlives the call. A failure leaves the app a lone instance.
    unsafe {
        RegisterClassExW(&class);
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            PCWSTR(name.as_ptr()),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance),
            None,
        );
    }
}

/// The window procedure of the window other starts hand URLs to.
unsafe extern "system" fn links_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg != WM_COPYDATA {
        // SAFETY: the default procedure, with what the system passed.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    // SAFETY: for `WM_COPYDATA`, `lparam` points at a `COPYDATASTRUCT` whose
    // `cbData` bytes at `lpData` the system copied into this process for
    // the duration of the call.
    let url = unsafe {
        let data = &*(lparam.0 as *const COPYDATASTRUCT);
        if data.dwData != URL_DATA || data.lpData.is_null() {
            return LRESULT(0);
        }
        let units = std::slice::from_raw_parts(data.lpData as *const u16, data.cbData as usize / 2);
        String::from_utf16(units).ok()
    };
    let Some(url) = url.filter(|url| is_link(url, declared_schemes())) else {
        return LRESULT(0);
    };
    let Some(queue) = QUEUE.with(|q| q.borrow().upgrade()) else {
        return LRESULT(0);
    };
    let Ok(mut pump) = queue.try_borrow_mut() else {
        return LRESULT(0);
    };
    pump.events.push_back(RawEvent::OpenUrl { url });
    let front = pump
        .windows
        .iter()
        .find(|w| w.alive.get())
        .map(|w| w.hwnd.get());
    drop(pump);
    // SAFETY: plain calls on this thread's own window and queue. The sent
    // message is handled inside the pump's wait, which a posted message
    // ends, so the queued URL is delivered.
    unsafe {
        if let Some(front) = front {
            if IsIconic(front).as_bool() {
                let _ = ShowWindow(front, SW_RESTORE);
            }
            let _ = SetForegroundWindow(front);
        }
        let _ = PostThreadMessageW(GetCurrentThreadId(), WM_WAKE, WPARAM(0), LPARAM(0));
    }
    LRESULT(1)
}

/// Whether the app runs from an installed package, whose manifest declares
/// its schemes.
fn packaged() -> bool {
    let mut length = 0u32;
    // SAFETY: asks only for the length, into a live out-pointer.
    unsafe { GetCurrentPackageFullName(&mut length, None) != APPMODEL_ERROR_NO_PACKAGE }
}

/// Registers this executable as the handler of every declared scheme under
/// the user's classes, where it is not already.
fn register_schemes() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let command = format!("\"{}\" \"%1\"", exe.display());
    for scheme in declared_schemes() {
        let key = format!("Software\\Classes\\{scheme}");
        let open = wide(&format!("{key}\\shell\\open\\command"));
        if read_string(&open).as_deref() == Some(command.as_str()) {
            continue;
        }
        let key = wide(&key);
        set_string(&key, None, &format!("URL:{scheme}"));
        set_string(&key, Some(w!("URL Protocol")), "");
        set_string(&open, None, &command);
    }
}

/// The default string of the user's key `key`, a NUL-terminated path.
fn read_string(key: &[u16]) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: `key` is NUL-terminated; `buf` holds `size` bytes, which the
    // call fills with a NUL-terminated string.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            None,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1).min(buf.len());
    String::from_utf16(&buf[..len]).ok()
}

/// Sets the string `value` (the key's default when `None`) of the user's key
/// `key`, creating the key.
fn set_string(key: &[u16], value: Option<PCWSTR>, text: &str) {
    let data = wide(text);
    // SAFETY: `key`, `value` and `data` are NUL-terminated and outlive the
    // call; `data` holds the byte count given.
    unsafe {
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            value.unwrap_or(PCWSTR::null()),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        );
    }
}
