//! The Win32 side of `lpk-gui-sys`: file IDs, the no-replace move, the password dialog, the
//! confirmation box and the shell's open verb. The only module of the crate allowed `unsafe` (docs/UNSAFE.md); every
//! block carries a `// SAFETY:` comment.

use std::cell::RefCell;
use std::ffi::{c_void, OsStr};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::sync::Once;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, DeleteObject, FillRect, SetBkColor, SetTextColor,
    COLOR_BTNFACE, HBRUSH, HDC, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::{
    FileIdInfo, GetFileInformationByHandleEx, MoveFileExW, FILE_ID_INFO, MOVEFILE_WRITE_THROUGH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow, SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetDlgItem,
    GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    IsDialogMessageW, LoadCursorW, MessageBoxW, PostQuitMessage, RegisterClassExW, SendMessageW,
    SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage, CREATESTRUCTW, CW_USEDEFAULT,
    ES_AUTOHSCROLL, ES_PASSWORD, GWLP_USERDATA, HMENU, IDCANCEL, IDC_ARROW, IDOK, IDYES,
    MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO, MSG, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS,
    SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT,
    WM_CTLCOLORSTATIC, WM_ERASEBKGND, WM_NCCREATE, WM_SETFONT, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};

use crate::{FileIdentity, PasswordTexts, Zeroizing};

fn io_err(e: windows::core::Error) -> io::Error {
    let code = e.code().0 as u32;
    if code >> 16 == 0x8007 {
        io::Error::from_raw_os_error((code & 0xFFFF) as i32)
    } else {
        io::Error::other(e.message())
    }
}

/// A NUL-terminated UTF-16 copy of `s`.
fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// A path for the `W` APIs: absolute, `\\?\`-prefixed (so no 260-character limit), NUL-terminated.
fn wide_path(p: &Path) -> io::Result<Vec<u16>> {
    let abs = std::path::absolute(p)?;
    let s = abs.as_os_str();
    if s.encode_wide().take(2).eq([u16::from(b'\\'); 2]) {
        // Already verbatim (`\\?\`) or a UNC path (`\\server\share`, left as it is).
        return Ok(wide(s));
    }
    let mut v: Vec<u16> = r"\\?\".encode_utf16().collect();
    v.extend(s.encode_wide());
    v.push(0);
    Ok(v)
}

pub(crate) fn file_identity(file: &File) -> io::Result<FileIdentity> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: the handle is the open file's and stays valid for this borrow of `file`; the
    // buffer is a `FILE_ID_INFO`, of the size passed, that outlives the call.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast::<c_void>(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .map_err(io_err)?;
    Ok(FileIdentity {
        volume: info.VolumeSerialNumber,
        file_id: u128::from_le_bytes(info.FileId.Identifier),
    })
}

pub(crate) fn move_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let f = wide_path(from)?;
    let t = wide_path(to)?;
    // SAFETY: both buffers are NUL-terminated UTF-16 strings alive for the call. Without
    // MOVEFILE_REPLACE_EXISTING the call fails when `to` exists.
    unsafe {
        MoveFileExW(
            PCWSTR(f.as_ptr()),
            PCWSTR(t.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(io_err)
}

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

pub(crate) fn shell_open(owner: Option<isize>, path: &Path) -> io::Result<()> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    // Not `\\?\`-prefixed: handlers receive the path as it is, and many cannot read that form.
    let file = wide(std::path::absolute(path)?.as_os_str());
    let verb = wide(OsStr::new("open"));
    // SAFETY: the strings are NUL-terminated and alive for the call; no parameters and no
    // directory (null); the owner, if any, is a window handle the caller passed.
    let r = unsafe {
        ShellExecuteW(
            owner.map(hwnd),
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values above 32 mean success (the documented contract of the return value).
    if r.0 as isize > 32 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "the shell could not open {} (code {})",
            path.display(),
            r.0 as isize
        )))
    }
}

pub(crate) fn confirm(owner: Option<isize>, title: &str, text: &str) -> bool {
    let t = wide(OsStr::new(title));
    let x = wide(OsStr::new(text));
    // SAFETY: the strings are NUL-terminated and alive for the call; the owner, if any, is a
    // window handle the caller passed (an invalid one makes the box unowned, not unsafe).
    let r = unsafe {
        MessageBoxW(
            owner.map(hwnd),
            PCWSTR(x.as_ptr()),
            PCWSTR(t.as_ptr()),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    r == IDYES
}

const CLASS_NAME: &str = "LitePackPasswordDialog";
const ID_FIRST: i32 = 101;
const ID_SECOND: i32 = 102;
const ID_STATUS: i32 = 103;
const DARK_BG: COLORREF = COLORREF(0x0020_2020);
const DARK_TEXT: COLORREF = COLORREF(0x00F0_F0F0);

/// What the window procedure shares with the modal loop (through `GWLP_USERDATA`): read-only
/// but for the result, a `RefCell` because a nested message loop may re-enter the procedure.
struct DialogState {
    confirm: bool,
    mismatch: Vec<u16>,
    brush: Option<HBRUSH>,
    result: RefCell<Option<Option<Zeroizing<String>>>>,
}

fn class_name() -> Vec<u16> {
    wide(OsStr::new(CLASS_NAME))
}

fn module() -> HINSTANCE {
    // SAFETY: `None` asks for the executable's own module handle, which needs no release.
    unsafe { GetModuleHandleW(None) }
        .map(|m| HINSTANCE(m.0))
        .unwrap_or_default()
}

fn register_class() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let name = class_name();
        // The class name must outlive every window of the class: leak this one small buffer.
        let name: &'static [u16] = Vec::leak(name);
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: module(),
            // SAFETY: a system cursor, loaded by its predefined ID with no module.
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut c_void),
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        // SAFETY: `wc` is fully initialised and its class name is 'static. A failure (the class
        // already registered) surfaces as CreateWindowExW's error.
        unsafe { RegisterClassExW(&wc) };
    });
}

/// The text of the edit `id` of `dlg`, read into wiped memory, the control emptied after.
fn take_text(dlg: HWND, id: i32) -> Zeroizing<String> {
    // SAFETY: `dlg` is our live dialog; GetDlgItem returns its child or an error.
    let Ok(edit) = (unsafe { GetDlgItem(Some(dlg), id) }) else {
        return Zeroizing::new(String::new());
    };
    // SAFETY: `edit` is a live child window.
    let len = unsafe { GetWindowTextLengthW(edit) }.max(0) as usize;
    let mut buf = Zeroizing::new(vec![0u16; len + 1]);
    // SAFETY: the buffer is writable for its whole length, which the call is told.
    let n = unsafe { GetWindowTextW(edit, &mut buf) }.max(0) as usize;
    // Room for the worst case up front, so the string never reallocates (and never leaves an
    // unwiped copy behind).
    let mut s = Zeroizing::new(String::with_capacity(n * 3));
    for c in char::decode_utf16(buf[..n.min(buf.len())].iter().copied()) {
        s.push(c.unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    set_text(dlg, id, &[0]);
    s
}

fn set_text(dlg: HWND, id: i32, text: &[u16]) {
    // SAFETY: `dlg` is our live dialog; `text` is NUL-terminated and alive for the call.
    unsafe {
        if let Ok(c) = GetDlgItem(Some(dlg), id) {
            let _ = SetWindowTextW(c, PCWSTR(text.as_ptr()));
        }
    }
}

fn focus(dlg: HWND, id: i32) {
    // SAFETY: `dlg` is our live dialog.
    unsafe {
        if let Ok(c) = GetDlgItem(Some(dlg), id) {
            let _ = SetFocus(Some(c));
        }
    }
}

fn on_ok(dlg: HWND, state: &DialogState) {
    let first = take_text(dlg, ID_FIRST);
    if state.confirm {
        let second = take_text(dlg, ID_SECOND);
        if *first != *second {
            set_text(dlg, ID_STATUS, &state.mismatch);
            // SAFETY: `dlg` is our live dialog and ID_STATUS its child.
            unsafe {
                if let Ok(c) = GetDlgItem(Some(dlg), ID_STATUS) {
                    let _ = ShowWindow(c, SW_SHOW);
                }
            }
            focus(dlg, ID_FIRST);
            return;
        }
    }
    *state.result.borrow_mut() = Some(Some(first));
}

extern "system" fn wndproc(dlg: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: for WM_NCCREATE, `lparam` points at the CREATESTRUCTW of this CreateWindowExW
        // call, whose lpCreateParams is the `DialogState` that `password_prompt` keeps alive
        // until the window is destroyed.
        unsafe {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(dlg, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(dlg, msg, wparam, lparam);
        }
    }
    // SAFETY: GWLP_USERDATA is 0 or the pointer stored above, valid while the window lives.
    let ptr = unsafe { GetWindowLongPtrW(dlg, GWLP_USERDATA) } as *const DialogState;
    if ptr.is_null() {
        // SAFETY: the default procedure, with the arguments the system passed.
        return unsafe { DefWindowProcW(dlg, msg, wparam, lparam) };
    }
    // SAFETY: see above; only shared references are ever made to the state.
    let state = unsafe { &*ptr };
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            if id == IDOK.0 {
                on_ok(dlg, state);
            } else if id == IDCANCEL.0 {
                *state.result.borrow_mut() = Some(None);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            *state.result.borrow_mut() = Some(None);
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT if state.brush.is_some() => {
            let hdc = HDC(wparam.0 as *mut c_void);
            // SAFETY: for these messages `wparam` is the control's device context.
            unsafe {
                SetTextColor(hdc, DARK_TEXT);
                SetBkColor(hdc, DARK_BG);
            }
            LRESULT(state.brush.map_or(0, |b| b.0 as isize))
        }
        WM_ERASEBKGND if state.brush.is_some() => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let mut rc = RECT::default();
            // SAFETY: `wparam` is the window's device context; `rc` is a valid out-parameter.
            unsafe {
                let _ = GetClientRect(dlg, &mut rc);
                if let Some(b) = state.brush {
                    FillRect(hdc, &rc, b);
                }
            }
            LRESULT(1)
        }
        // SAFETY: the default procedure, with the arguments the system passed.
        _ => unsafe { DefWindowProcW(dlg, msg, wparam, lparam) },
    }
}

/// Test hook: called with the dialog once it is shown, before the loop runs.
pub(crate) type Hook<'a> = Option<&'a dyn Fn(HWND)>;

pub(crate) fn password_prompt(
    owner: Option<isize>,
    texts: &PasswordTexts,
    dark: bool,
    hook: Hook<'_>,
) -> io::Result<Option<Zeroizing<String>>> {
    register_class();
    let owner = owner.map(hwnd);
    let dpi = match owner {
        // SAFETY: a window handle the caller passed; an invalid one returns 0.
        Some(o) => unsafe { GetDpiForWindow(o) },
        None => 0,
    };
    let dpi = if dpi == 0 {
        // SAFETY: no argument.
        unsafe { GetDpiForSystem() }
    } else {
        dpi
    };
    let px = |v: i32| v * dpi as i32 / 96;

    // SAFETY: CreateSolidBrush takes a colour; the brush is deleted below.
    let brush = dark.then(|| unsafe { CreateSolidBrush(DARK_BG) });
    let state = Box::new(DialogState {
        confirm: texts.confirm.is_some(),
        mismatch: wide(OsStr::new(&texts.mismatch)),
        brush,
        result: RefCell::new(None),
    });

    // The layout, in 96-DPI units scaled to the monitor's DPI (the process is per-monitor
    // DPI aware through tao).
    let (w, margin, label_h, edit_h, btn_w, btn_h) = (360, 12, 20, 24, 88, 26);
    let mut y = margin;
    let mut controls: Vec<(&str, String, WINDOW_STYLE, WINDOW_EX_STYLE, i32, RECT)> = Vec::new();
    let rect = |x: i32, y: i32, cw: i32, ch: i32| RECT {
        left: x,
        top: y,
        right: cw,
        bottom: ch,
    };
    let label = WS_CHILD | WS_VISIBLE;
    let edit = WS_CHILD
        | WS_VISIBLE
        | WS_TABSTOP
        | WINDOW_STYLE(ES_PASSWORD as u32)
        | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
    let none = WINDOW_EX_STYLE(0);
    controls.push((
        "STATIC",
        texts.prompt.clone(),
        label,
        none,
        0,
        rect(margin, y, w - 2 * margin, label_h),
    ));
    y += label_h + 2;
    controls.push((
        "EDIT",
        String::new(),
        edit,
        WS_EX_CLIENTEDGE,
        ID_FIRST,
        rect(margin, y, w - 2 * margin, edit_h),
    ));
    y += edit_h + 10;
    if let Some(c) = &texts.confirm {
        controls.push((
            "STATIC",
            c.clone(),
            label,
            none,
            0,
            rect(margin, y, w - 2 * margin, label_h),
        ));
        y += label_h + 2;
        controls.push((
            "EDIT",
            String::new(),
            edit,
            WS_EX_CLIENTEDGE,
            ID_SECOND,
            rect(margin, y, w - 2 * margin, edit_h),
        ));
        y += edit_h + 6;
        // Hidden until the two fields differ.
        controls.push((
            "STATIC",
            String::new(),
            WS_CHILD,
            none,
            ID_STATUS,
            rect(margin, y, w - 2 * margin, label_h),
        ));
        y += label_h + 4;
    }
    let button = WS_CHILD | WS_VISIBLE | WS_TABSTOP;
    // BS_DEFPUSHBUTTON = 1: Enter presses OK (IsDialogMessageW).
    controls.push((
        "BUTTON",
        texts.ok.clone(),
        button | WINDOW_STYLE(1),
        none,
        IDOK.0,
        rect(w - margin - 2 * btn_w - 8, y, btn_w, btn_h),
    ));
    controls.push((
        "BUTTON",
        texts.cancel.clone(),
        button,
        none,
        IDCANCEL.0,
        rect(w - margin - btn_w, y, btn_w, btn_h),
    ));
    let client_h = y + btn_h + margin;

    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let ex = WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT;
    let mut frame = RECT {
        left: 0,
        top: 0,
        right: px(w),
        bottom: px(client_h),
    };
    // SAFETY: `frame` is a valid in/out rectangle.
    let _ = unsafe { AdjustWindowRectExForDpi(&mut frame, style, false, ex, dpi) };
    let (fw, fh) = (frame.right - frame.left, frame.bottom - frame.top);
    let (mut x0, mut y0) = (CW_USEDEFAULT, CW_USEDEFAULT);
    if let Some(o) = owner {
        let mut or = RECT::default();
        // SAFETY: `or` is a valid out-parameter; a stale handle fails and keeps the default.
        if unsafe { GetWindowRect(o, &mut or) }.is_ok() {
            x0 = or.left + (or.right - or.left - fw) / 2;
            y0 = or.top + (or.bottom - or.top - fh) / 2;
        }
    }

    let class = class_name();
    let title = wide(OsStr::new(&texts.title));
    let state_ptr: *const DialogState = &*state;
    // SAFETY: the class is registered; strings are NUL-terminated and alive for the call;
    // `state_ptr` stays valid until after DestroyWindow below (the Box lives to the end).
    let dlg = unsafe {
        CreateWindowExW(
            ex,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
            x0,
            y0,
            fw,
            fh,
            owner,
            None,
            Some(module()),
            Some(state_ptr.cast::<c_void>()),
        )
    }
    .map_err(io_err)?;

    // The message font at this DPI.
    let mut ncm = NONCLIENTMETRICSW {
        cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: `ncm` is a NONCLIENTMETRICSW whose size the call is told.
    let font = unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            ncm.cbSize,
            Some((&mut ncm as *mut NONCLIENTMETRICSW).cast::<c_void>()),
            0,
            dpi,
        )
    }
    .ok()
    // SAFETY: a LOGFONTW the system filled in; the font is deleted below.
    .map(|()| unsafe { CreateFontIndirectW(&ncm.lfMessageFont) });

    let dark_on: i32 = 1;
    if dark {
        // SAFETY: the attribute takes a BOOL-sized value, passed by pointer with its size.
        let _ = unsafe {
            DwmSetWindowAttribute(
                dlg,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&dark_on as *const i32).cast::<c_void>(),
                size_of::<i32>() as u32,
            )
        };
    }
    for (class, text, style, ex, id, r) in &controls {
        let c = wide(OsStr::new(class));
        let t = wide(OsStr::new(text));
        // SAFETY: strings NUL-terminated and alive; the parent is our live dialog; the menu
        // argument of a child window is its control ID.
        let child = unsafe {
            CreateWindowExW(
                *ex,
                PCWSTR(c.as_ptr()),
                PCWSTR(t.as_ptr()),
                *style,
                px(r.left),
                px(r.top),
                px(r.right),
                px(r.bottom),
                Some(dlg),
                Some(HMENU(*id as isize as *mut c_void)),
                Some(module()),
                None,
            )
        };
        let Ok(child) = child else { continue };
        if let Some(f) = font {
            // SAFETY: WM_SETFONT takes the font handle and a redraw flag.
            unsafe {
                SendMessageW(
                    child,
                    WM_SETFONT,
                    Some(WPARAM(f.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
        }
        if dark {
            let theme = if *class == "EDIT" {
                "DarkMode_CFD"
            } else {
                "DarkMode_Explorer"
            };
            let th = wide(OsStr::new(theme));
            // SAFETY: a NUL-terminated theme name alive for the call; a missing theme is ignored.
            let _ = unsafe { SetWindowTheme(child, PCWSTR(th.as_ptr()), PCWSTR::null()) };
        }
        if *id == ID_STATUS {
            // SAFETY: our live child window.
            let _ = unsafe { ShowWindow(child, SW_HIDE) };
        }
    }

    if let Some(o) = owner {
        // SAFETY: a window handle the caller passed; disabling it makes the dialog modal.
        let _ = unsafe { EnableWindow(o, false) };
    }
    // SAFETY: our live dialog.
    let _ = unsafe { ShowWindow(dlg, SW_SHOW) };
    focus(dlg, ID_FIRST);
    if let Some(h) = hook {
        h(dlg);
    }

    let mut msg = MSG::default();
    let mut failure = None;
    while state.result.borrow().is_none() {
        // SAFETY: `msg` is a valid out-parameter.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
        if r == 0 {
            // WM_QUIT: give it back to the outer loop and cancel.
            // SAFETY: no pointer argument.
            unsafe { PostQuitMessage(msg.wParam.0 as i32) };
            *state.result.borrow_mut() = Some(None);
            break;
        }
        if r == -1 {
            failure = Some(io::Error::last_os_error());
            break;
        }
        // SAFETY: `msg` came from GetMessageW; `dlg` is our live dialog.
        unsafe {
            if !IsDialogMessageW(dlg, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    // Wipe whatever is still in the fields before the window goes.
    drop(take_text(dlg, ID_FIRST));
    drop(take_text(dlg, ID_SECOND));
    if let Some(o) = owner {
        // SAFETY: re-enable the owner before destroying the dialog, so focus returns to it.
        let _ = unsafe { EnableWindow(o, true) };
    }
    // SAFETY: our dialog, destroyed once; its procedure sees no state after this.
    unsafe {
        SetWindowLongPtrW(dlg, GWLP_USERDATA, 0);
        let _ = DestroyWindow(dlg);
    }
    // SAFETY: the GDI objects created above, no longer selected anywhere.
    unsafe {
        if let Some(f) = font {
            let _ = DeleteObject(HGDIOBJ(f.0));
        }
        if let Some(b) = brush {
            let _ = DeleteObject(HGDIOBJ(b.0));
        }
    }
    if let Some(e) = failure {
        return Err(e);
    }
    let r = state.result.borrow_mut().take();
    Ok(r.flatten())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

    fn texts(confirm: bool) -> PasswordTexts {
        PasswordTexts {
            title: "Password (test)".into(),
            prompt: "Password".into(),
            confirm: confirm.then(|| "Again".into()),
            ok: "OK".into(),
            cancel: "Cancel".into(),
            mismatch: "The passwords differ.".into(),
        }
    }

    fn press(dlg: HWND, id: i32) {
        // SAFETY: a message posted to our own live dialog.
        unsafe { PostMessageW(Some(dlg), WM_COMMAND, WPARAM(id as usize), LPARAM(0)) }.unwrap();
    }

    fn type_into(dlg: HWND, id: i32, s: &str) {
        set_text(dlg, id, &wide(OsStr::new(s)));
    }

    #[test]
    fn ok_returns_the_typed_password() {
        let hook = |dlg: HWND| {
            type_into(dlg, ID_FIRST, "pässwörd ✓");
            type_into(dlg, ID_SECOND, "pässwörd ✓");
            press(dlg, IDOK.0);
        };
        for dark in [false, true] {
            let got = password_prompt(None, &texts(true), dark, Some(&hook)).unwrap();
            assert_eq!(got.as_deref().map(String::as_str), Some("pässwörd ✓"));
        }
    }

    #[test]
    fn cancel_returns_none() {
        let hook = |dlg: HWND| {
            type_into(dlg, ID_FIRST, "secret");
            press(dlg, IDCANCEL.0);
        };
        assert!(password_prompt(None, &texts(false), false, Some(&hook))
            .unwrap()
            .is_none());
    }

    #[test]
    fn two_different_entries_keep_the_dialog_open() {
        // OK with differing fields must not return; the Cancel queued after it then does.
        let hook = |dlg: HWND| {
            type_into(dlg, ID_FIRST, "one");
            type_into(dlg, ID_SECOND, "two");
            press(dlg, IDOK.0);
            press(dlg, IDCANCEL.0);
        };
        assert!(password_prompt(None, &texts(true), false, Some(&hook))
            .unwrap()
            .is_none());
    }

    #[test]
    fn the_wide_path_is_verbatim_and_terminated() {
        let p = wide_path(Path::new(r"C:\x\y")).unwrap();
        assert_eq!(String::from_utf16_lossy(&p), "\\\\?\\C:\\x\\y\0");
        let v = wide_path(Path::new(r"\\?\C:\x")).unwrap();
        assert_eq!(String::from_utf16_lossy(&v), "\\\\?\\C:\\x\0");
    }
}
