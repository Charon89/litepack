//! The COM boundary of `lpk_shell.dll` (`docs/notes/e6-design.md` sections 1 and 2), the only
//! module of the crate allowed `unsafe` (docs/UNSAFE.md; D-120 of E6-1).
//!
//! It exports `DllGetClassObject` and `DllCanUnloadNow`, and holds the class factory, the root
//! `IExplorerCommand` "LitePack" (with `IObjectWithSite`), its enumerator and the seven verb
//! commands (E6-2). The root and the verbs read only names and `SFGAO` attributes of the selected
//! items; `Invoke` writes the response file of [`crate::protocol`] and starts the GUI, nothing
//! else. The registry read and the process start sit behind [`Host`] so that tests never touch
//! the registry or start a process. Nothing here registers the class.
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

use std::cell::{Cell, RefCell};
use std::ffi::{c_void, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_NOTIMPL, E_POINTER, E_UNEXPECTED,
    S_FALSE, S_OK,
};
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx, IClassFactory, IClassFactory_Impl};
use windows::Win32::System::Ole::{IObjectWithSite, IObjectWithSite_Impl};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
use windows::Win32::System::SystemServices::{SFGAO_FOLDER, SFGAO_STREAM};
use windows::Win32::System::Threading::{
    CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IEnumExplorerCommand_Impl, IExplorerCommand, IExplorerCommand_Impl,
    IFolderView, IPersistFolder2, IShellItem, IShellItemArray, IUnknown_QueryService,
    SHGetNameFromIDList, SHStrDupW, ECF_HASSUBCOMMANDS, ECS_ENABLED, ECS_HIDDEN, SIGDN_FILESYSPATH,
};
use windows_core::{
    implement, w, Error, IUnknown, Interface, Ref, Result, BOOL, GUID, HRESULT, PCWSTR, PWSTR,
};

use crate::model::{classify, leaf, request, title, visible, Entry, Selection, Verb, ROOT_TITLE};
use crate::protocol;

/// The class ID of LitePack's Explorer command, `{39B4D969-AE7A-4965-84DB-2DAF0C5B0308}`.
/// E6-3's package manifest registers it.
pub const CLSID_LPK_SHELL: GUID = GUID::from_u128(0x39b4d969_ae7a_4965_84db_2daf0c5b0308);

/// The class ID of the same command under route C, the classic registration that
/// `lpk-gui.exe --register-shell` writes under `HKCU\Software\Classes` (E6-4):
/// `{01E42821-7D51-4B9B-AD9F-FFCA769A6025}`, distinct from [`CLSID_LPK_SHELL`] so that the package
/// and the classic route never share a class. `lpk-gui`'s `register::CLASSIC_CLSID` holds its text
/// (a test compares them); the DLL serves both with the same root.
pub const CLSID_LPK_SHELL_CLASSIC: GUID = GUID::from_u128(0x01e42821_7d51_4b9b_ad9f_ffca769a6025);

/// The GUI's local-state folder name (`lpk-gui`'s `paths::IDENTIFIER`, whose test compares it with
/// Tauri's identifier) and the folder of response files under it.
const GUI_STATE_FOLDER: &str = "org.litepack.gui";
const VERBS_FOLDER: &str = "Verbs";

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

/// What the commands take from the outside world: the registry, the process start and the
/// background folder. [`RealHost`] is the production one; tests replace it so that no test reads
/// the real registry or starts a process.
pub trait Host {
    /// `HKCU\Software\LitePack`'s `GuiPath` value, as recorded (not checked here).
    fn gui_path(&self) -> Option<PathBuf>;
    /// The folder of response files, `%LOCALAPPDATA%\org.litepack.gui\Verbs`.
    fn verbs_dir(&self) -> Option<PathBuf>;
    /// Start `app` with the command line `command_line` (UTF-16, no NUL) and `cwd` as working
    /// directory, inheriting no handles.
    fn launch(&self, app: &Path, command_line: &[u16], cwd: &Path) -> Result<()>;
    /// The folder shown by the root's site (`SID_SFolderView`, `IFolderView::GetFolder`), as a
    /// file-system path in UTF-16.
    fn site_folder(&self, site: &IUnknown) -> Option<Vec<u16>>;
}

/// The production [`Host`].
#[derive(Debug, Clone, Copy)]
struct RealHost;

/// A NUL-terminated copy of `text` for a Win32 call.
fn nul_terminated(text: &[u16]) -> Vec<u16> {
    text.iter().copied().chain(std::iter::once(0)).collect()
}

/// A string the shell allocated, copied out and freed.
fn take_pwstr(p: PWSTR) -> Option<Vec<u16>> {
    if p.is_null() {
        return None;
    }
    // SAFETY: `p` is a NUL-terminated string the shell returned in CoTaskMemAlloc memory; it is
    // read once here and freed once, and not used afterwards.
    let text = unsafe { p.as_wide() }.to_vec();
    // SAFETY: the string came from CoTaskMemAlloc, which is what the shell's calls document.
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    Some(text)
}

impl Host for RealHost {
    fn gui_path(&self) -> Option<PathBuf> {
        let mut size = 0u32;
        // SAFETY: the key, subkey and value names are NUL-terminated literals; a null data
        // pointer with a size pointer only asks for the size in bytes.
        let first = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!("Software\\LitePack"),
                w!("GuiPath"),
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut size),
            )
        };
        if first.0 != 0 || !(2..=65_536).contains(&size) {
            return None;
        }
        let mut buf = vec![0u16; usize::try_from(size).ok()? / 2 + 1];
        let mut size = u32::try_from(buf.len() * 2).ok()?;
        // SAFETY: `buf` is writable for `size` bytes, which is what the call is told.
        let second = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!("Software\\LitePack"),
                w!("GuiPath"),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if second.0 != 0 {
            return None;
        }
        let units = usize::try_from(size).ok()? / 2;
        let text = buf.get(..units.min(buf.len()))?;
        let end = text.iter().position(|&u| u == 0).unwrap_or(text.len());
        let text = text.get(..end)?;
        (!text.is_empty()).then(|| PathBuf::from(OsString::from_wide(text)))
    }

    fn verbs_dir(&self) -> Option<PathBuf> {
        let local = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty())?;
        Some(
            PathBuf::from(local)
                .join(GUI_STATE_FOLDER)
                .join(VERBS_FOLDER),
        )
    }

    fn launch(&self, app: &Path, command_line: &[u16], cwd: &Path) -> Result<()> {
        let app = nul_terminated(&app.as_os_str().encode_wide().collect::<Vec<_>>());
        let cwd = nul_terminated(&cwd.as_os_str().encode_wide().collect::<Vec<_>>());
        // CreateProcessW may write into the command line, so it gets its own buffer.
        let mut line = nul_terminated(command_line);
        let startup = STARTUPINFOW {
            cb: u32::try_from(std::mem::size_of::<STARTUPINFOW>()).unwrap_or(0),
            ..STARTUPINFOW::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: `app`, `cwd` and `line` are NUL-terminated and outlive the call; `line` is
        // writable; no security attributes, environment or inherited handles are passed;
        // `startup` has `cb` set and `info` is a writable out structure.
        unsafe {
            CreateProcessW(
                PCWSTR(app.as_ptr()),
                Some(PWSTR(line.as_mut_ptr())),
                None,
                None,
                false,
                PROCESS_CREATION_FLAGS(0),
                None,
                PCWSTR(cwd.as_ptr()),
                &startup,
                &mut info,
            )?;
        }
        // SAFETY: on success both handles are open and ours; each is closed once.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(info.hThread);
            let _ = windows::Win32::Foundation::CloseHandle(info.hProcess);
        }
        Ok(())
    }

    fn site_folder(&self, site: &IUnknown) -> Option<Vec<u16>> {
        // SAFETY: COM calls on live interfaces; SID_SFolderView is IFolderView's own IID.
        let view: IFolderView = unsafe { IUnknown_QueryService(site, &IFolderView::IID) }.ok()?;
        // SAFETY: as above.
        let folder: IPersistFolder2 = unsafe { view.GetFolder() }.ok()?;
        // SAFETY: as above; the item ID list returned is ours to free.
        let pidl = unsafe { folder.GetCurFolder() }.ok()?;
        // SAFETY: `pidl` is a valid item ID list from GetCurFolder.
        let name = unsafe { SHGetNameFromIDList(pidl, SIGDN_FILESYSPATH) };
        // SAFETY: GetCurFolder's list is CoTaskMemAlloc memory, freed once here.
        unsafe { CoTaskMemFree(Some(pidl as *const c_void)) };
        take_pwstr(name.ok()?)
    }
}

/// The site Explorer gave the root, shared with the commands the root enumerates.
type Site = Rc<RefCell<Option<IUnknown>>>;

/// How many items the array holds (none: the folder background).
fn item_count(items: Option<&IShellItemArray>) -> Result<usize> {
    match items {
        None => Ok(0),
        // SAFETY: a COM call on a live interface.
        Some(a) => Ok(usize::try_from(unsafe { a.GetCount() }?).unwrap_or(usize::MAX)),
    }
}

fn item_at(items: &IShellItemArray, index: usize) -> Option<IShellItem> {
    // SAFETY: a COM call on a live interface.
    unsafe { items.GetItemAt(u32::try_from(index).ok()?) }.ok()
}

/// The item's file-system path; none for an item without one (a library, This PC, a ZIP's
/// inside).
fn item_path(item: &IShellItem) -> Option<Vec<u16>> {
    // SAFETY: a COM call on a live interface.
    let p = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok()?;
    take_pwstr(p).filter(|t| !t.is_empty())
}

/// What the menu reads of one item: its path's names and `SFGAO_FOLDER` / `SFGAO_STREAM`.
fn item_entry(item: &IShellItem) -> Option<Entry> {
    let path = String::from_utf16_lossy(&item_path(item)?);
    // SAFETY: a COM call on a live interface.
    let attrs = unsafe { item.GetAttributes(SFGAO_FOLDER | SFGAO_STREAM) }.ok()?;
    let trimmed = path.trim_end_matches(['\\', '/']);
    let name = leaf(&path).to_owned();
    let parent = trimmed
        .get(..trimmed.len().saturating_sub(name.len()))
        .unwrap_or("")
        .trim_end_matches(['\\', '/'])
        .to_owned();
    Some(Entry {
        name,
        // A ZIP is a folder and a stream to Explorer: it is a file here.
        is_dir: attrs.contains(SFGAO_FOLDER) && !attrs.contains(SFGAO_STREAM),
        parent,
    })
}

/// Classify the selection, reading items lazily; `None` hides the menu.
fn selection(items: Option<&IShellItemArray>) -> Option<Selection> {
    let count = item_count(items).ok()?;
    classify(
        count,
        (0..count).map(|i| {
            items
                .and_then(|a| item_at(a, i))
                .and_then(|it| item_entry(&it))
        }),
    )
}

/// `hide` or `show` as the state of a command.
fn state_of(shown: bool) -> u32 {
    let state = if shown { ECS_ENABLED } else { ECS_HIDDEN };
    u32::try_from(state.0).unwrap_or(0)
}

/// A copy of `text` in `CoTaskMemAlloc` memory, as `IExplorerCommand` returns strings (the
/// caller frees it).
fn co_task_string(text: &str) -> Result<PWSTR> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and alive for the whole call; SHStrDupW only reads it and
    // returns a new CoTaskMemAlloc copy that the COM caller owns.
    unsafe { SHStrDupW(PCWSTR(wide.as_ptr())) }
}

/// The command line the GUI is started with: `"<GuiPath>" @<file>`, the `@` argument quoted only
/// when the file's path holds a space or a tab (a user name may).
fn command_line(gui: &Path, file: &Path) -> Vec<u16> {
    let mut line: Vec<u16> = vec![u16::from(b'"')];
    line.extend(gui.as_os_str().encode_wide());
    line.extend([u16::from(b'"'), u16::from(b' ')]);
    let file: Vec<u16> = file.as_os_str().encode_wide().collect();
    let quote = file
        .iter()
        .any(|&u| u == u16::from(b' ') || u == u16::from(b'\t'));
    if quote {
        line.push(u16::from(b'"'));
    }
    line.push(u16::from(b'@'));
    line.extend(file);
    if quote {
        line.push(u16::from(b'"'));
    }
    line
}

/// Run one verb: write the response file, start the GUI. Nothing is written when the GUI is not
/// registered; the file is removed again when the start fails.
fn invoke(host: &dyn Host, site: &Site, verb: Verb, items: Option<&IShellItemArray>) -> Result<()> {
    let gui = host
        .gui_path()
        .filter(|p| p.is_absolute() && p.is_file())
        .ok_or_else(|| Error::from(HRESULT::from_win32(2)))?;
    let sel = selection(items).ok_or_else(|| Error::from(E_FAIL))?;
    let req = request(verb, &sel).ok_or_else(|| Error::from(E_FAIL))?;
    let mut paths: Vec<Vec<u16>> = Vec::with_capacity(req.items.len().max(1));
    if req.background {
        let folder = site
            .try_borrow()
            .ok()
            .and_then(|s| s.as_ref().and_then(|s| host.site_folder(s)))
            .ok_or_else(|| Error::from(E_FAIL))?;
        paths.push(folder);
    }
    for &i in &req.items {
        let path = items
            .and_then(|a| item_at(a, i))
            .and_then(|it| item_path(&it))
            .ok_or_else(|| Error::from(E_FAIL))?;
        paths.push(path);
    }
    let name: Vec<u16> = req
        .name
        .as_deref()
        .map(|n| n.encode_utf16().collect())
        .unwrap_or_default();
    let bytes = protocol::encode(req.flag, &name, &paths).ok_or_else(|| Error::from(E_FAIL))?;
    let dir = host.verbs_dir().ok_or_else(|| Error::from(E_FAIL))?;
    let file = protocol::write_new(&dir, &bytes)?;
    let cwd = gui.parent().unwrap_or(Path::new(".")).to_path_buf();
    if let Err(e) = host.launch(&gui, &command_line(&gui, &file), &cwd) {
        let _ = std::fs::remove_file(&file);
        return Err(e);
    }
    Ok(())
}

/// The "LitePack" command: the root of the submenu, holding the seven verbs.
#[implement(IExplorerCommand, IObjectWithSite, Agile = false)]
struct ExplorerCommand {
    host: Rc<dyn Host>,
    site: Site,
    _live: Live,
}

impl ExplorerCommand {
    fn new() -> Self {
        Self::with_host(Rc::new(RealHost))
    }

    fn with_host(host: Rc<dyn Host>) -> Self {
        ExplorerCommand {
            host,
            site: Rc::new(RefCell::new(None)),
            _live: Live::new(),
        }
    }
}

#[allow(non_snake_case)]
impl IExplorerCommand_Impl for ExplorerCommand_Impl {
    fn GetTitle(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        guard(|| co_task_string(ROOT_TITLE))
    }

    fn GetIcon(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        guard(|| {
            let gui = self
                .host
                .gui_path()
                .filter(|p| p.is_absolute() && p.is_file())
                .ok_or_else(|| Error::from(E_NOTIMPL))?;
            let mut icon: Vec<u16> = vec![u16::from(b'"')];
            icon.extend(gui.as_os_str().encode_wide());
            icon.extend("\",0".encode_utf16());
            co_task_string(&String::from_utf16_lossy(&icon))
        })
    }

    fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        guard(|| Err(E_NOTIMPL.into()))
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        guard(|| Err(E_NOTIMPL.into()))
    }

    fn GetState(&self, items: Ref<IShellItemArray>, _ok_to_be_slow: BOOL) -> Result<u32> {
        guard(|| Ok(state_of(selection(items.as_ref()).is_some())))
    }

    fn Invoke(&self, _items: Ref<IShellItemArray>, _bind: Ref<IBindCtx>) -> Result<()> {
        guard(|| Err(E_NOTIMPL.into()))
    }

    fn GetFlags(&self) -> Result<u32> {
        guard(|| Ok(u32::try_from(ECF_HASSUBCOMMANDS.0).unwrap_or(0)))
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        guard(|| {
            Ok(VerbEnum {
                host: self.host.clone(),
                site: self.site.clone(),
                next: Cell::new(0),
                _live: Live::new(),
            }
            .into())
        })
    }
}

#[allow(non_snake_case)]
impl IObjectWithSite_Impl for ExplorerCommand_Impl {
    fn SetSite(&self, site: Ref<IUnknown>) -> Result<()> {
        guard(|| {
            let mut slot = self
                .site
                .try_borrow_mut()
                .map_err(|_| Error::from(E_FAIL))?;
            *slot = site.as_ref().cloned();
            Ok(())
        })
    }

    fn GetSite(&self, riid: *const GUID, site: *mut *mut c_void) -> Result<()> {
        guard(|| {
            if riid.is_null() || site.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: `site` is non-null and writable by IObjectWithSite's contract.
            unsafe { *site = std::ptr::null_mut() };
            let slot = self.site.try_borrow().map_err(|_| Error::from(E_FAIL))?;
            let held = slot.as_ref().ok_or_else(|| Error::from(E_FAIL))?;
            // SAFETY: `riid` is non-null and points to the caller's IID; `site` is writable.
            unsafe { held.query(riid, site) }.ok()
        })
    }
}

/// One of the seven verbs, a subcommand of the root.
#[implement(IExplorerCommand, Agile = false)]
struct VerbCommand {
    verb: Verb,
    host: Rc<dyn Host>,
    site: Site,
    _live: Live,
}

#[allow(non_snake_case)]
impl IExplorerCommand_Impl for VerbCommand_Impl {
    fn GetTitle(&self, items: Ref<IShellItemArray>) -> Result<PWSTR> {
        guard(|| {
            let text = match selection(items.as_ref()) {
                Some(sel) => title(self.verb, &sel),
                None => self.verb.text().to_owned(),
            };
            co_task_string(&text)
        })
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

    fn GetState(&self, items: Ref<IShellItemArray>, _ok_to_be_slow: BOOL) -> Result<u32> {
        guard(|| {
            let shown = selection(items.as_ref()).is_some_and(|s| visible(s.kind, self.verb));
            Ok(state_of(shown))
        })
    }

    fn Invoke(&self, items: Ref<IShellItemArray>, _bind: Ref<IBindCtx>) -> Result<()> {
        guard(|| invoke(&*self.host, &self.site, self.verb, items.as_ref()))
    }

    fn GetFlags(&self) -> Result<u32> {
        guard(|| Ok(0))
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        guard(|| Err(E_NOTIMPL.into()))
    }
}

/// The enumerator over the seven verbs.
#[implement(IEnumExplorerCommand, Agile = false)]
struct VerbEnum {
    host: Rc<dyn Host>,
    site: Site,
    next: Cell<usize>,
    _live: Live,
}

#[allow(non_snake_case)]
impl IEnumExplorerCommand_Impl for VerbEnum_Impl {
    fn Next(
        &self,
        celt: u32,
        commands: *mut Option<IExplorerCommand>,
        fetched: *mut u32,
    ) -> HRESULT {
        match guard(|| {
            if commands.is_null() && celt > 0 {
                return Err(E_POINTER.into());
            }
            let mut got = 0u32;
            while got < celt {
                let Some(&verb) = Verb::ALL.get(self.next.get()) else {
                    break;
                };
                let command: IExplorerCommand = VerbCommand {
                    verb,
                    host: self.host.clone(),
                    site: self.site.clone(),
                    _live: Live::new(),
                }
                .into();
                // SAFETY: `commands` points to `celt` writable, possibly uninitialised slots by
                // IEnumExplorerCommand's contract and `got < celt`; `write` does not drop the
                // old contents.
                unsafe {
                    commands
                        .add(usize::try_from(got).unwrap_or(0))
                        .write(Some(command))
                };
                got += 1;
                self.next.set(self.next.get() + 1);
            }
            if !fetched.is_null() {
                // SAFETY: `fetched` is non-null and writable by the contract.
                unsafe { *fetched = got };
            }
            Ok(got == celt)
        }) {
            Ok(true) => S_OK,
            Ok(false) => S_FALSE,
            Err(e) => e.code(),
        }
    }

    fn Skip(&self, celt: u32) -> Result<()> {
        guard(|| {
            let to = self
                .next
                .get()
                .saturating_add(usize::try_from(celt).unwrap_or(0));
            self.next.set(to.min(Verb::ALL.len()));
            Ok(())
        })
    }

    fn Reset(&self) -> Result<()> {
        guard(|| {
            self.next.set(0);
            Ok(())
        })
    }

    fn Clone(&self) -> Result<IEnumExplorerCommand> {
        guard(|| {
            Ok(VerbEnum {
                host: self.host.clone(),
                site: self.site.clone(),
                next: Cell::new(self.next.get()),
                _live: Live::new(),
            }
            .into())
        })
    }
}

/// The class factory of [`CLSID_LPK_SHELL`].
#[implement(IClassFactory, Agile = false)]
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
            if object.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: `object` is non-null and, by IClassFactory's contract, points to a
            // writable interface pointer; it is cleared before any other check so that every
            // failure, a null `riid` included, leaves null.
            unsafe { *object = std::ptr::null_mut() };
            if riid.is_null() {
                return Err(E_POINTER.into());
            }
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

/// The DLL's class-object entry point: the class factory of [`CLSID_LPK_SHELL`] or of
/// [`CLSID_LPK_SHELL_CLASSIC`] (the same root), or `CLASS_E_CLASSNOTAVAILABLE` for any other class.
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
        if clsid != CLSID_LPK_SHELL && clsid != CLSID_LPK_SHELL_CLASSIC {
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
#[path = "com_tests.rs"]
mod tests;
