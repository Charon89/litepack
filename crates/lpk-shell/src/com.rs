//! The COM boundary of `lpk_shell.dll` (`docs/notes/e6-design.md` section 1), the only module of
//! the crate allowed `unsafe` (docs/UNSAFE.md; D-NN of E6-1).
//!
//! It exports `DllGetClassObject` and `DllCanUnloadNow`, and holds the class factory and the
//! `IExplorerCommand` object. In E6-1 the command gives the fixed title [`ROOT_TITLE`] and
//! answers every other method with `E_NOTIMPL`; E6-2 adds the subcommands, the states and
//! `Invoke`. Nothing here registers the class or writes to the registry.
//!
//! The code runs inside Explorer and any other program showing a shell menu: every exported
//! function and COM method body runs under [`guard`], which turns a panic into `E_UNEXPECTED`
//! instead of unwinding out of an `extern "system"` frame (which would abort the host), and a
//! build with `panic = "abort"` fails to compile.

#![allow(unsafe_code)]

#[cfg(panic = "abort")]
compile_error!(
    "lpk-shell runs inside Explorer: it needs panic = \"unwind\" so that guard() can turn a \
     panic into E_UNEXPECTED (docs/notes/e6-design.md section 1)"
);

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_NOTIMPL, E_POINTER, E_UNEXPECTED, S_FALSE,
    S_OK,
};
use windows::Win32::System::Com::{IBindCtx, IClassFactory, IClassFactory_Impl};
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IExplorerCommand, IExplorerCommand_Impl, IShellItemArray, SHStrDupW,
};
use windows_core::{implement, Interface, Ref, Result, BOOL, GUID, HRESULT, PCWSTR, PWSTR};

use crate::model::ROOT_TITLE;

/// The class ID of LitePack's Explorer command, `{39B4D969-AE7A-4965-84DB-2DAF0C5B0308}`.
/// E6-3's package manifest and E6-4's route C (under a second class ID) register it.
pub const CLSID_LPK_SHELL: GUID = GUID::from_u128(0x39b4d969_ae7a_4965_84db_2daf0c5b0308);

/// Live COM objects (commands and class factories) created by this DLL.
static OBJECTS: AtomicUsize = AtomicUsize::new(0);
/// Outstanding `IClassFactory::LockServer(TRUE)` calls.
static LOCKS: AtomicUsize = AtomicUsize::new(0);

/// Runs a COM method body, turning a panic into `E_UNEXPECTED`.
fn guard<T>(body: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|_| Err(E_UNEXPECTED.into()))
}

fn hresult(result: Result<()>) -> HRESULT {
    match result {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

/// Counts one live object for `DllCanUnloadNow` while it exists.
struct Live;

impl Live {
    fn new() -> Self {
        OBJECTS.fetch_add(1, Ordering::SeqCst);
        Live
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The "LitePack" command. E6-1: a fixed title, everything else `E_NOTIMPL`.
#[implement(IExplorerCommand)]
struct ExplorerCommand {
    _live: Live,
}

impl ExplorerCommand {
    fn new() -> Self {
        ExplorerCommand { _live: Live::new() }
    }
}

/// A copy of `text` in `CoTaskMemAlloc` memory, as `IExplorerCommand` returns strings (the
/// caller frees it).
fn co_task_string(text: &str) -> Result<PWSTR> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and alive for the whole call; SHStrDupW only reads it and
    // returns a new CoTaskMemAlloc copy that the COM caller owns.
    unsafe { SHStrDupW(PCWSTR(wide.as_ptr())) }
}

#[allow(non_snake_case)]
impl IExplorerCommand_Impl for ExplorerCommand_Impl {
    fn GetTitle(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        guard(|| co_task_string(ROOT_TITLE))
    }

    fn GetIcon(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        guard(|| Err(E_NOTIMPL.into()))
    }

    fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        guard(|| Err(E_NOTIMPL.into()))
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        guard(|| Err(E_NOTIMPL.into()))
    }

    fn GetState(&self, _items: Ref<IShellItemArray>, _ok_to_be_slow: BOOL) -> Result<u32> {
        guard(|| Err(E_NOTIMPL.into()))
    }

    fn Invoke(&self, _items: Ref<IShellItemArray>, _bind: Ref<IBindCtx>) -> Result<()> {
        guard(|| Err(E_NOTIMPL.into()))
    }

    fn GetFlags(&self) -> Result<u32> {
        guard(|| Err(E_NOTIMPL.into()))
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        guard(|| Err(E_NOTIMPL.into()))
    }
}

/// The class factory of [`CLSID_LPK_SHELL`].
#[implement(IClassFactory)]
struct ClassFactory {
    _live: Live,
}

#[allow(non_snake_case)]
impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<windows_core::IUnknown>,
        riid: *const GUID,
        object: *mut *mut c_void,
    ) -> Result<()> {
        guard(|| {
            if object.is_null() || riid.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: `object` is non-null and, by IClassFactory's contract, points to a
            // writable interface pointer; it is cleared first so that every failure leaves null.
            unsafe { *object = std::ptr::null_mut() };
            if !outer.is_null() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            let command: windows_core::IUnknown = ExplorerCommand::new().into();
            // SAFETY: `riid` is non-null and points to the caller's IID; `object` is writable
            // (above). `query` writes an AddRef'd pointer or null and returns its HRESULT.
            unsafe { command.query(riid, object) }.ok()
        })
    }

    fn LockServer(&self, lock: BOOL) -> Result<()> {
        guard(|| {
            if lock.as_bool() {
                LOCKS.fetch_add(1, Ordering::SeqCst);
            } else {
                // An unbalanced unlock is ignored rather than wrapping the count.
                let mut n = LOCKS.load(Ordering::SeqCst);
                while n > 0 {
                    match LOCKS.compare_exchange(n, n - 1, Ordering::SeqCst, Ordering::SeqCst) {
                        Ok(_) => break,
                        Err(now) => n = now,
                    }
                }
            }
            Ok(())
        })
    }
}

/// The DLL's class-object entry point: the class factory of [`CLSID_LPK_SHELL`], or
/// `CLASS_E_CLASSNOTAVAILABLE` for any other class.
///
/// # Safety
///
/// COM's contract for `DllGetClassObject`: `rclsid` and `riid` are null or point to a GUID,
/// and `ppv` is null or points to a writable interface pointer.
#[no_mangle]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    hresult(guard(|| {
        if ppv.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: `ppv` is non-null and writable by this function's contract; it is cleared
        // first so that every failure leaves null.
        unsafe { *ppv = std::ptr::null_mut() };
        if rclsid.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: `rclsid` is non-null and points to a GUID by this function's contract.
        let clsid = unsafe { *rclsid };
        if clsid != CLSID_LPK_SHELL {
            return Err(CLASS_E_CLASSNOTAVAILABLE.into());
        }
        let factory: windows_core::IUnknown = ClassFactory { _live: Live::new() }.into();
        // SAFETY: `riid` is non-null and points to the caller's IID; `ppv` is writable (above).
        unsafe { factory.query(riid, ppv) }.ok()
    }))
}

/// `S_OK` when no object of this DLL is alive and no `LockServer(TRUE)` is outstanding, so COM
/// may unload it; `S_FALSE` otherwise.
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::SeqCst) == 0 && LOCKS.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[cfg(test)]
mod tests {
    //! The COM objects called in process (no registration): one test, because the object and
    //! lock counts are process-wide and the test harness runs tests on several threads.
    #![allow(clippy::expect_used)]

    use super::*;
    use windows::Win32::System::Com::CoTaskMemFree;

    fn class_object(clsid: &GUID) -> HRESULT {
        let mut raw = std::ptr::null_mut();
        // SAFETY: both GUIDs and the out pointer are valid for the call.
        let hr = unsafe { DllGetClassObject(clsid, &IClassFactory::IID, &mut raw) };
        if hr.is_ok() {
            // SAFETY: on success `raw` is an AddRef'd IClassFactory; from_raw takes that reference.
            drop(unsafe { IClassFactory::from_raw(raw) });
        } else {
            assert!(raw.is_null());
        }
        hr
    }

    #[test]
    fn the_skeleton_answers_as_designed() {
        // Another class is refused; null arguments are refused.
        let other = GUID::from_u128(0x0000_0000_0000_0000_c000_0000_0000_0046);
        assert_eq!(class_object(&other), CLASS_E_CLASSNOTAVAILABLE);
        // SAFETY: null pointers are part of the contract and must be refused.
        let hr = unsafe {
            DllGetClassObject(&CLSID_LPK_SHELL, &IClassFactory::IID, std::ptr::null_mut())
        };
        assert_eq!(hr, E_POINTER);
        assert_eq!(class_object(&CLSID_LPK_SHELL), S_OK);
        assert_eq!(DllCanUnloadNow(), S_OK);

        let mut raw = std::ptr::null_mut();
        // SAFETY: valid GUIDs and out pointer.
        let hr = unsafe { DllGetClassObject(&CLSID_LPK_SHELL, &IClassFactory::IID, &mut raw) };
        assert_eq!(hr, S_OK);
        // SAFETY: on success `raw` is an AddRef'd IClassFactory.
        let factory = unsafe { IClassFactory::from_raw(raw) };
        assert_eq!(DllCanUnloadNow(), S_FALSE);

        // Aggregation is refused.
        let outer: windows_core::IUnknown = factory.cast().expect("IUnknown");
        // SAFETY: a COM call on a live interface.
        let aggregated = unsafe { factory.CreateInstance::<_, IExplorerCommand>(&outer) };
        assert_eq!(
            aggregated.err().map(|e| e.code()),
            Some(CLASS_E_NOAGGREGATION)
        );
        drop(outer);

        // SAFETY: a COM call on a live interface.
        let command: IExplorerCommand =
            unsafe { factory.CreateInstance(None::<&windows_core::IUnknown>) }.expect("command");

        // The fixed title, in CoTaskMemAlloc memory.
        // SAFETY: a COM call on a live interface.
        let title = unsafe { command.GetTitle(None) }.expect("title");
        // SAFETY: GetTitle returned a NUL-terminated string that we own until CoTaskMemFree.
        let text = unsafe { title.to_string() }.expect("UTF-16");
        // SAFETY: the string came from CoTaskMemAlloc (SHStrDupW) and is freed once.
        unsafe { CoTaskMemFree(Some(title.0 as *const c_void)) };
        assert_eq!(text, ROOT_TITLE);

        // Everything else is not implemented yet (E6-2).
        // SAFETY: COM calls on a live interface with null optional arguments.
        unsafe {
            assert_eq!(
                command.GetIcon(None).err().map(|e| e.code()),
                Some(E_NOTIMPL)
            );
            assert_eq!(
                command.GetToolTip(None).err().map(|e| e.code()),
                Some(E_NOTIMPL)
            );
            assert_eq!(
                command.GetCanonicalName().err().map(|e| e.code()),
                Some(E_NOTIMPL)
            );
            assert_eq!(
                command.GetState(None, false).err().map(|e| e.code()),
                Some(E_NOTIMPL)
            );
            assert_eq!(
                command.Invoke(None, None).err().map(|e| e.code()),
                Some(E_NOTIMPL)
            );
            assert_eq!(command.GetFlags().err().map(|e| e.code()), Some(E_NOTIMPL));
            assert_eq!(
                command.EnumSubCommands().err().map(|e| e.code()),
                Some(E_NOTIMPL)
            );
        }

        // The DLL stays loaded while a lock is held, even with no object alive.
        // SAFETY: COM calls on a live interface.
        unsafe { factory.LockServer(true) }.expect("lock");
        drop(command);
        drop(factory);
        assert_eq!(DllCanUnloadNow(), S_FALSE);
        assert_eq!(OBJECTS.load(Ordering::SeqCst), 0);
        LOCKS.fetch_sub(1, Ordering::SeqCst);
        assert_eq!(DllCanUnloadNow(), S_OK);

        // A panic inside a method body becomes E_UNEXPECTED.
        let caught: Result<()> = guard(|| unreachable!("a panic in a method body"));
        assert_eq!(caught.err().map(|e| e.code()), Some(E_UNEXPECTED));
    }
}
