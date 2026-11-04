//! The COM objects called in process (no registration, no registry, no process): selections of
//! each kind are built from real temporary files and folders as shell items, and the commands
//! are called through their interfaces. The object and lock counts are process-wide and the test
//! harness runs tests on several threads, so every test holds [`SERIAL`].
#![allow(clippy::expect_used, clippy::indexing_slicing)]

use super::*;
use crate::model::Verb;
use std::sync::Mutex;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{SHCreateShellItemArrayFromIDLists, SHParseDisplayName};
use windows_core::HSTRING;

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn init_com() {
    // SAFETY: initialises COM on this test thread; a repeat call (S_FALSE) is harmless.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
}

/// A fresh scratch folder.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        Self::under(&std::env::temp_dir())
    }

    fn under(base: &Path) -> Self {
        let dir = base.join(format!("lpk-shell-com-{}", protocol::random_hex()));
        std::fs::create_dir_all(&dir).expect("scratch");
        // Shell items give paths back in long form; a runner's temp folder may be an 8.3 short
        // name (`RUNNER~1`), so the expected paths are built from the long form.
        let long = std::fs::canonicalize(&dir).expect("canonical scratch");
        let text = long.to_string_lossy();
        let long = match text.strip_prefix("\\\\?\\") {
            Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
            _ => long,
        };
        Scratch(long)
    }

    fn file(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, b"x").expect("file");
        p
    }

    fn dir(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::create_dir_all(&p).expect("dir");
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A shell item array over parse names (paths, or `shell:::{...}` for an item without one).
fn array_of(names: &[String]) -> IShellItemArray {
    array_with(names.iter().map(String::as_str).collect::<Vec<_>>(), 1)
}

/// `repeat` times each of `names`, sharing one item ID list per name (cheap for big counts).
fn array_with(names: Vec<&str>, repeat: usize) -> IShellItemArray {
    let mut pidls = Vec::new();
    for n in &names {
        let mut pidl = std::ptr::null_mut();
        // SAFETY: the name is a NUL-terminated HSTRING and the out pointer is writable.
        unsafe { SHParseDisplayName(&HSTRING::from(*n), None, &mut pidl, 0, None) }
            .expect("parse display name");
        pidls.push(pidl);
    }
    let list: Vec<*const _> = (0..repeat)
        .flat_map(|_| pidls.iter().map(|p| p.cast_const()))
        .collect();
    // SAFETY: every pointer in `list` is a valid item ID list that outlives the call.
    let array = unsafe { SHCreateShellItemArrayFromIDLists(&list) }.expect("array");
    for p in pidls {
        // SAFETY: each list came from SHParseDisplayName (CoTaskMemAlloc) and is freed once.
        unsafe { CoTaskMemFree(Some(p as *const c_void)) };
    }
    array
}

fn names(paths: &[&PathBuf]) -> Vec<String> {
    paths.iter().map(|p| p.display().to_string()).collect()
}

fn text(p: PWSTR) -> String {
    String::from_utf16_lossy(&take_pwstr(p).expect("a string"))
}

/// The root through the class factory, as Explorer creates it.
fn root_from_factory() -> (IClassFactory, IExplorerCommand) {
    let mut raw = std::ptr::null_mut();
    // SAFETY: valid GUIDs and out pointer.
    let hr = unsafe { DllGetClassObject(&CLSID_LPK_SHELL, &IClassFactory::IID, &mut raw) };
    assert_eq!(hr, S_OK);
    // SAFETY: on success `raw` is an AddRef'd IClassFactory.
    let factory = unsafe { IClassFactory::from_raw(raw) };
    // SAFETY: a COM call on a live interface.
    let command: IExplorerCommand =
        unsafe { factory.CreateInstance(None::<&windows_core::IUnknown>) }.expect("command");
    (factory, command)
}

/// The seven subcommands of a root, in enumeration order.
fn verbs_of(root: &IExplorerCommand) -> Vec<IExplorerCommand> {
    // SAFETY: COM calls on live interfaces; `buf` has room for the eight requested slots.
    unsafe {
        let enumerator = root.EnumSubCommands().expect("enumerator");
        let mut buf: [Option<IExplorerCommand>; 8] = std::array::from_fn(|_| None);
        let mut fetched = 0u32;
        let hr = enumerator.Next(&mut buf, Some(&mut fetched));
        assert_eq!((hr, fetched), (S_FALSE, 7));
        assert_eq!(
            enumerator.Next(&mut buf, Some(&mut fetched)),
            S_FALSE,
            "exhausted"
        );
        assert_eq!(fetched, 0);
        enumerator.Reset().expect("reset");
        let copy = enumerator.Clone().expect("clone");
        enumerator.Skip(3).expect("skip");
        let mut one: [Option<IExplorerCommand>; 1] = [None];
        assert_eq!(enumerator.Next(&mut one, None), S_OK);
        // Out parameters: a fresh slot, as an earlier value would be overwritten unreleased.
        let mut two: [Option<IExplorerCommand>; 1] = [None];
        assert_eq!(
            copy.Next(&mut two, None),
            S_OK,
            "the clone has its own position"
        );
        buf.into_iter().flatten().collect()
    }
}

fn state(command: &IExplorerCommand, items: &IShellItemArray) -> u32 {
    // SAFETY: a COM call on live interfaces.
    unsafe { command.GetState(items, false) }.expect("state")
}

fn title_of(command: &IExplorerCommand, items: &IShellItemArray) -> String {
    // SAFETY: a COM call on live interfaces.
    text(unsafe { command.GetTitle(items) }.expect("title"))
}

const HIDDEN: u32 = 2;
const ENABLED: u32 = 0;

#[test]
fn the_factory_and_the_object_counts() {
    let _g = serial();
    // Another class is refused; null arguments are refused.
    let other = GUID::from_u128(0x0000_0000_0000_0000_c000_0000_0000_0046);
    let mut raw = std::ptr::null_mut();
    // SAFETY: valid GUIDs and out pointer.
    let hr = unsafe { DllGetClassObject(&other, &IClassFactory::IID, &mut raw) };
    assert_eq!(hr, CLASS_E_CLASSNOTAVAILABLE);
    assert!(raw.is_null());
    // SAFETY: null pointers are part of the contract and must be refused.
    let hr =
        unsafe { DllGetClassObject(&CLSID_LPK_SHELL, &IClassFactory::IID, std::ptr::null_mut()) };
    assert_eq!(hr, E_POINTER);
    assert_eq!(DllCanUnloadNow(), S_OK);

    let (factory, command) = root_from_factory();
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

    // The root's own answers.
    // SAFETY: COM calls on a live interface with null optional arguments.
    unsafe {
        assert_eq!(text(command.GetTitle(None).expect("title")), ROOT_TITLE);
        assert_eq!(command.GetFlags().expect("flags"), 1, "ECF_HASSUBCOMMANDS");
        assert_eq!(
            command.GetToolTip(None).err().map(|e| e.code()),
            Some(E_NOTIMPL)
        );
        assert_eq!(
            command.Invoke(None, None).err().map(|e| e.code()),
            Some(E_NOTIMPL)
        );
    }
    let site = command.cast::<IObjectWithSite>().expect("IObjectWithSite");
    // SAFETY: COM calls on live interfaces.
    unsafe {
        assert!(site.GetSite::<IUnknown>().is_err(), "no site yet");
        let some: IUnknown = command.cast().expect("IUnknown");
        site.SetSite(&some).expect("set site");
        assert!(site.GetSite::<IUnknown>().is_ok());
        site.SetSite(None::<&IUnknown>).expect("clear site");
        assert!(site.GetSite::<IUnknown>().is_err());
    }
    // Every verb command is a leaf with the default flags.
    for v in verbs_of(&command) {
        // SAFETY: COM calls on a live interface.
        unsafe {
            assert_eq!(v.GetFlags().expect("flags"), 0);
            assert_eq!(v.EnumSubCommands().err().map(|e| e.code()), Some(E_NOTIMPL));
        }
    }

    // The DLL stays loaded while a lock is held, even with no object alive.
    // SAFETY: COM calls on a live interface.
    unsafe { factory.LockServer(true) }.expect("lock");
    drop(site);
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

/// `[Add to archive, Add to "<name>.lpk", Extract here, Extract to, Open, Test, Convert]`.
type Shown = [bool; 7];

#[test]
fn each_selection_kind_shows_the_designs_verbs_with_computed_titles() {
    let _g = serial();
    init_com();
    let s = Scratch::new();
    let work = s.dir("work");
    let put = |name: &str| {
        let p = work.join(name);
        std::fs::write(&p, b"x").expect("file");
        p
    };
    let notes = put("notes.txt");
    let pic = put("pic.png");
    let lpk = put("x.lpk");
    let sevenz = put("y.7z");
    let zip = put("z.zip");
    let vol1 = put("big.7z.001");
    let vol2 = put("big.7z.002");
    let amp = put("Fish & Chips.txt");
    let bidi = put("a\u{202E}gpj.txt");
    let long = put(&format!("{}.txt", "n".repeat(60)));
    let folder = s.dir("My Folder");

    let (_factory, root) = root_from_factory();
    let verbs = verbs_of(&root);
    assert_eq!(verbs.len(), 7);

    let none = [false; 7];
    let _ = none;
    struct Case {
        items: Vec<String>,
        shown: Shown,
        titles: [&'static str; 7],
    }
    let t = |a: &'static str, b: &'static str, c: &'static str| -> [&'static str; 7] {
        [
            "Add to archive\u{2026}",
            a,
            "Extract here",
            b,
            "Open",
            "Test",
            c,
        ]
    };
    let cases = vec![
        // F: two files (name: their folder's).
        Case {
            items: names(&[&notes, &pic]),
            shown: [true, true, false, false, false, false, false],
            titles: t(
                "Add to \"work.lpk\"",
                "Extract to \"work\\\"",
                "Convert to\u{2026}",
            ),
        },
        // F: one file, named less its extension.
        Case {
            items: names(&[&notes]),
            shown: [true, true, false, false, false, false, false],
            titles: t(
                "Add to \"notes.lpk\"",
                "Extract to \"notes\\\"",
                "Convert to\u{2026}",
            ),
        },
        // D: a folder.
        Case {
            items: names(&[&folder]),
            shown: [true, true, false, false, false, false, false],
            titles: t(
                "Add to \"My Folder.lpk\"",
                "Extract to \"My Folder\\\"",
                "Convert to\u{2026}",
            ),
        },
        // L: one .lpk.
        Case {
            items: names(&[&lpk]),
            shown: [true, false, true, true, true, true, false],
            titles: t(
                "Add to \"x.lpk\"",
                "Extract to \"x\\\"",
                "Convert to\u{2026}",
            ),
        },
        // A: one foreign archive, and a ZIP (a folder and stream to Explorer) is one too.
        Case {
            items: names(&[&sevenz]),
            shown: [true, false, true, true, true, true, true],
            titles: t(
                "Add to \"y.lpk\"",
                "Extract to \"y\\\"",
                "Convert to\u{2026}",
            ),
        },
        Case {
            items: names(&[&zip]),
            shown: [true, false, true, true, true, true, true],
            titles: t(
                "Add to \"z.lpk\"",
                "Extract to \"z\\\"",
                "Convert to\u{2026}",
            ),
        },
        // A: one multi-volume set.
        Case {
            items: names(&[&vol1, &vol2]),
            shown: [true, false, true, true, true, true, true],
            titles: t(
                "Add to \"big.lpk\"",
                "Extract to \"big\\\"",
                "Convert to\u{2026}",
            ),
        },
        // S: several archives.
        Case {
            items: names(&[&lpk, &sevenz]),
            shown: [true, false, true, true, false, true, false],
            titles: t(
                "Add to \"work.lpk\"",
                "Extract to \"*\\\"",
                "Convert to\u{2026}",
            ),
        },
        // M: an archive with another file; files with a folder.
        Case {
            items: names(&[&sevenz, &notes]),
            shown: [true, true, false, false, false, false, false],
            titles: t(
                "Add to \"work.lpk\"",
                "Extract to \"work\\\"",
                "Convert to\u{2026}",
            ),
        },
        Case {
            items: names(&[&notes, &folder]),
            shown: [true, true, false, false, false, false, false],
            titles: t(
                "Add to \"work.lpk\"",
                "Extract to \"work\\\"",
                "Convert to\u{2026}",
            ),
        },
        // Names with `&`, a bidirectional character and more than 40 characters.
        Case {
            items: names(&[&amp]),
            shown: [true, true, false, false, false, false, false],
            titles: t(
                "Add to \"Fish && Chips.lpk\"",
                "Extract to \"Fish && Chips\\\"",
                "Convert to\u{2026}",
            ),
        },
        Case {
            items: names(&[&bidi]),
            shown: [true, true, false, false, false, false, false],
            titles: t(
                "Add to \"a\\u{202E}gpj.lpk\"",
                "Extract to \"a\\u{202E}gpj\\\"",
                "Convert to\u{2026}",
            ),
        },
    ];
    for case in cases {
        let array = array_of(&case.items);
        // SAFETY: a COM call on live interfaces.
        assert_eq!(state(&root, &array), ENABLED, "{:?}", case.items);
        for (i, v) in verbs.iter().enumerate() {
            let want = if case.shown[i] { ENABLED } else { HIDDEN };
            assert_eq!(state(v, &array), want, "{:?} verb {i}", case.items);
            if case.shown[i] {
                assert_eq!(title_of(v, &array), case.titles[i], "{:?}", case.items);
            }
        }
    }

    // A name past 40 characters is shortened in the middle.
    let array = array_of(&names(&[&long]));
    let title = title_of(&verbs[1], &array);
    assert_eq!(
        title,
        format!(
            "Add to \"{}\u{2026}{}.lpk\"",
            "n".repeat(19),
            "n".repeat(20)
        )
    );

    // The folder background: no items; only `Add to archive...`.
    for (i, v) in verbs.iter().enumerate() {
        // SAFETY: COM calls on live interfaces.
        let st = unsafe { v.GetState(None, false) }.expect("state");
        assert_eq!(
            st,
            if i == 0 { ENABLED } else { HIDDEN },
            "background verb {i}"
        );
    }
    // SAFETY: a COM call on a live interface.
    let root_state = unsafe { root.GetState(None, false) }.expect("state");
    assert_eq!(root_state, ENABLED);

    // An item without a file-system path (This PC) hides the whole menu.
    let this_pc = array_with(vec!["shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"], 1);
    assert_eq!(state(&root, &this_pc), HIDDEN);
    for v in &verbs {
        assert_eq!(state(v, &this_pc), HIDDEN);
    }
    // ... also when only one item of several has none.
    let mixed = array_with(
        vec![
            &notes.display().to_string(),
            "shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}",
        ],
        1,
    );
    assert_eq!(state(&root, &mixed), HIDDEN);
}

#[test]
fn past_16384_items_only_add_to_archive_remains() {
    let _g = serial();
    init_com();
    let s = Scratch::new();
    let f = s.file("a.txt");
    let (_factory, root) = root_from_factory();
    let verbs = verbs_of(&root);
    let many = array_with(vec![&f.display().to_string()], crate::model::MAX_ITEMS + 1);
    for (i, v) in verbs.iter().enumerate() {
        let want = if i == 0 { ENABLED } else { HIDDEN };
        assert_eq!(state(v, &many), want, "verb {i}");
    }
    assert_eq!(title_of(&verbs[0], &many), "Add to archive\u{2026}");
    // At the bound itself the selection is still read as an ordinary one.
    let at = array_with(vec![&f.display().to_string()], crate::model::MAX_ITEMS);
    assert_eq!(state(&verbs[1], &at), ENABLED);
}

/// A host that records what it would have launched and never touches the registry.
struct TestHost {
    gui: Option<PathBuf>,
    dir: PathBuf,
    folder: Option<Vec<u16>>,
    fail_launch: bool,
    launched: Mutex<Vec<(PathBuf, String, PathBuf)>>,
}

impl Host for TestHost {
    fn gui_path(&self) -> Option<PathBuf> {
        self.gui.clone()
    }

    fn verbs_dir(&self) -> Option<PathBuf> {
        Some(self.dir.clone())
    }

    fn launch(&self, app: &Path, command_line: &[u16], cwd: &Path) -> Result<()> {
        if self.fail_launch {
            return Err(E_FAIL.into());
        }
        self.launched.lock().expect("lock").push((
            app.to_path_buf(),
            String::from_utf16_lossy(command_line),
            cwd.to_path_buf(),
        ));
        Ok(())
    }

    fn site_folder(&self, _site: &IUnknown) -> Option<Vec<u16>> {
        self.folder.clone()
    }
}

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().collect()
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(rd) => rd.map(|e| e.expect("entry").path()).collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn invoke_writes_the_response_file_and_the_command_line() {
    let _g = serial();
    init_com();
    let s = Scratch::new();
    let gui = s.file("lpk gui.exe");
    let verbs_dir = s.0.join("local").join("Verbs");
    let work = s.dir("work");
    let put = |name: &str| {
        let p = work.join(name);
        std::fs::write(&p, b"x").expect("file");
        p
    };
    let notes = put("notes.txt");
    let lpk = put("x.lpk");
    let sevenz = put("y.7z");
    let vol1 = put("big.7z.001");
    let vol2 = put("big.7z.002");
    let host = Rc::new(TestHost {
        gui: Some(gui.clone()),
        dir: verbs_dir.clone(),
        folder: Some(wide(&work)),
        fail_launch: false,
        launched: Mutex::new(Vec::new()),
    });
    let root: IExplorerCommand = ExplorerCommand::with_host(host.clone()).into();
    let verbs = verbs_of(&root);
    let [create, named, here, to, open, test, convert] = &verbs[..] else {
        unreachable!("seven verbs")
    };

    // (command, selection, flag, name, the paths sent)
    type Row<'a> = (
        &'a IExplorerCommand,
        Vec<&'a PathBuf>,
        &'a str,
        &'a str,
        Vec<&'a PathBuf>,
    );
    let rows: Vec<Row> = vec![
        (
            create,
            vec![&notes, &lpk],
            "--create",
            "",
            vec![&notes, &lpk],
        ),
        (named, vec![&notes], "--create", "notes.lpk", vec![&notes]),
        (
            here,
            vec![&lpk, &sevenz],
            "--extract-here",
            "",
            vec![&lpk, &sevenz],
        ),
        (to, vec![&lpk], "--extract-to", "", vec![&lpk]),
        (open, vec![&sevenz], "--open", "", vec![&sevenz]),
        (test, vec![&lpk, &sevenz], "--test", "", vec![&lpk, &sevenz]),
        (convert, vec![&sevenz], "--convert", "", vec![&sevenz]),
        // One multi-volume set: the first volume stands for it.
        (open, vec![&vol2, &vol1], "--open", "", vec![&vol1]),
    ];
    for (command, selected, flag, name, sent) in rows {
        let array = array_of(&names(&selected));
        // SAFETY: a COM call on live interfaces.
        unsafe { command.Invoke(&array, None) }.expect("invoke");
        let launched = host.launched.lock().expect("lock").pop().expect("launched");
        let files = files_in(&verbs_dir);
        assert_eq!(files.len(), 1, "one response file per Invoke");
        let file = &files[0];
        let bytes = std::fs::read(file).expect("read");
        let paths: Vec<Vec<u16>> = sent.iter().map(|p| wide(p)).collect();
        let want = protocol::encode(flag, &name.encode_utf16().collect::<Vec<_>>(), &paths)
            .expect("encodes");
        assert_eq!(bytes, want, "{flag} {name}");
        let stem = file.file_name().and_then(|n| n.to_str()).expect("name");
        assert!(stem.starts_with("shell-") && stem.ends_with(".verbs") && stem.len() == 6 + 32 + 6);
        // `"<GuiPath>" @<file>`; the file's path holds a space (the scratch is under the user's
        // temp folder only by chance), so the `@` argument is quoted only then.
        let at = file.display().to_string();
        let arg = if at.contains(' ') {
            format!("\"@{at}\"")
        } else {
            format!("@{at}")
        };
        assert_eq!(launched.1, format!("\"{}\" {arg}", gui.display()));
        assert_eq!(launched.0, gui);
        assert_eq!(launched.2, s.0);
        std::fs::remove_file(file).expect("remove");
    }

    // The folder background: the path comes from the root's site, not from the items.
    let site = root.cast::<IObjectWithSite>().expect("site");
    let some: IUnknown = root.cast().expect("IUnknown");
    // SAFETY: COM calls on live interfaces.
    unsafe { site.SetSite(&some) }.expect("site");
    // SAFETY: a COM call on a live interface.
    unsafe { create.Invoke(None, None) }.expect("invoke background");
    let files = files_in(&verbs_dir);
    let want = protocol::encode("--create", &[], &[wide(&work)]).expect("encodes");
    assert_eq!(std::fs::read(&files[0]).expect("read"), want);
    std::fs::remove_file(&files[0]).expect("remove");
    // Another verb is hidden for the background: refused, nothing written.
    // SAFETY: a COM call on a live interface.
    assert!(unsafe { open.Invoke(None, None) }.is_err());
    assert!(files_in(&verbs_dir).is_empty());
    // SAFETY: a COM call on a live interface.
    unsafe { site.SetSite(None::<&IUnknown>) }.expect("clear");
    // No site: nothing to send, nothing written.
    // SAFETY: a COM call on a live interface.
    assert!(unsafe { create.Invoke(None, None) }.is_err());
    assert!(files_in(&verbs_dir).is_empty());
}

#[test]
fn invoke_starts_nothing_and_writes_nothing_without_a_usable_gui_path() {
    let _g = serial();
    init_com();
    let s = Scratch::new();
    let f = s.file("a.txt");
    let verbs_dir = s.0.join("Verbs");
    let array = array_of(&names(&[&f]));
    // A relative GuiPath is refused even when a file of that name exists from here.
    let relative = PathBuf::from("Cargo.toml");
    assert!(relative.is_file() && relative.is_relative());
    for gui in [
        None,
        Some(s.0.join("missing.exe")),
        Some(s.dir("a-folder")),
        Some(relative),
    ] {
        let host = Rc::new(TestHost {
            gui,
            dir: verbs_dir.clone(),
            folder: None,
            fail_launch: false,
            launched: Mutex::new(Vec::new()),
        });
        let root: IExplorerCommand = ExplorerCommand::with_host(host.clone()).into();
        let verbs = verbs_of(&root);
        // SAFETY: a COM call on live interfaces.
        assert!(unsafe { verbs[0].Invoke(&array, None) }.is_err());
        assert!(host.launched.lock().expect("lock").is_empty());
        assert!(!verbs_dir.exists(), "no folder and no file");
        // And no icon without a GuiPath.
        // SAFETY: a COM call on a live interface.
        assert!(unsafe { root.GetIcon(None) }.is_err());
    }
    // A failed start leaves no response file behind.
    let gui = s.file("lpk-gui.exe");
    let host = Rc::new(TestHost {
        gui: Some(gui.clone()),
        dir: verbs_dir.clone(),
        folder: None,
        fail_launch: true,
        launched: Mutex::new(Vec::new()),
    });
    let root: IExplorerCommand = ExplorerCommand::with_host(host).into();
    let verbs = verbs_of(&root);
    // SAFETY: a COM call on live interfaces.
    assert!(unsafe { verbs[0].Invoke(&array, None) }.is_err());
    assert!(files_in(&verbs_dir).is_empty());
    // The root's icon is `"<GuiPath>",0`.
    let host = Rc::new(TestHost {
        gui: Some(gui.clone()),
        dir: verbs_dir,
        folder: None,
        fail_launch: false,
        launched: Mutex::new(Vec::new()),
    });
    let root: IExplorerCommand = ExplorerCommand::with_host(host).into();
    // SAFETY: a COM call on a live interface.
    let icon = text(unsafe { root.GetIcon(None) }.expect("icon"));
    assert_eq!(icon, format!("\"{}\",0", gui.display()));
}

#[test]
fn the_command_line_quotes_the_file_only_when_it_has_a_space() {
    let line =
        |g: &str, f: &str| String::from_utf16_lossy(&command_line(Path::new(g), Path::new(f)));
    assert_eq!(
        line(
            "C:\\Program Files\\LitePack\\lpk-gui.exe",
            "C:\\Users\\a\\x.verbs"
        ),
        "\"C:\\Program Files\\LitePack\\lpk-gui.exe\" @C:\\Users\\a\\x.verbs"
    );
    assert_eq!(
        line("C:\\g.exe", "C:\\Users\\Jo Smith\\x.verbs"),
        "\"C:\\g.exe\" \"@C:\\Users\\Jo Smith\\x.verbs\""
    );
}

#[test]
fn a_verb_without_items_or_with_garbage_arguments_does_not_crash() {
    let _g = serial();
    init_com();
    // Through a test host: no test reaches the real registry read or the process start.
    let host = Rc::new(TestHost {
        gui: None,
        dir: std::env::temp_dir().join(format!("lpk-shell-none-{}", protocol::random_hex())),
        folder: None,
        fail_launch: false,
        launched: Mutex::new(Vec::new()),
    });
    let root: IExplorerCommand = ExplorerCommand::with_host(host).into();
    for v in verbs_of(&root) {
        // SAFETY: COM calls on a live interface with null optional arguments.
        unsafe {
            assert!(v.GetTitle(None).is_ok());
            let _ = v.GetState(None, true);
            let _ = v.Invoke(None, None);
        }
    }
}

/// The folder a real shell site reports is read through `SID_SFolderView`; a site that has no
/// folder view (any plain object) yields none rather than failing.
#[test]
fn the_real_host_reads_no_folder_from_a_plain_site() {
    let _g = serial();
    init_com();
    let (_factory, root) = root_from_factory();
    let plain: IUnknown = root.cast().expect("IUnknown");
    assert_eq!(RealHost.site_folder(&plain), None);
    let dir = RealHost.verbs_dir();
    assert!(dir.is_none_or(|d| d.ends_with("org.litepack.gui\\Verbs")));
}

#[test]
fn the_name_of_the_gui_state_folder_is_the_guis() {
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-gui/tauri.conf.json"),
    )
    .expect("tauri.conf.json");
    assert!(
        manifest.contains(&format!("\"identifier\": \"{GUI_STATE_FOLDER}\"")),
        "lpk-gui's identifier changed: update GUI_STATE_FOLDER"
    );
    assert_eq!(Verb::ALL.len(), 7);
}

#[test]
fn the_manifest_fragment_names_this_class_for_the_three_item_types() {
    let xml = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("manifest/file-explorer-context-menus.xml"),
    )
    .expect("the manifest fragment");
    assert!(xml.contains("Category=\"windows.fileExplorerContextMenus\""));
    for item in ["*", "Directory", "Directory\\Background"] {
        assert!(xml.contains(&format!("ItemType Type=\"{item}\"")), "{item}");
    }
    let clsid = format!("{:?}", CLSID_LPK_SHELL).to_uppercase();
    assert_eq!(
        xml.matches(&format!("Clsid=\"{clsid}\"")).count(),
        3,
        "{clsid}"
    );
}

#[test]
fn no_object_is_agile() {
    use windows::Win32::System::Com::IAgileObject;
    let _g = serial();
    init_com();
    let (factory, root) = root_from_factory();
    assert!(root.cast::<IAgileObject>().is_err(), "the root");
    assert!(factory.cast::<IAgileObject>().is_err(), "the factory");
    let verbs = verbs_of(&root);
    assert!(verbs[0].cast::<IAgileObject>().is_err(), "a verb");
    // SAFETY: COM calls on live interfaces.
    let enumerator = unsafe { root.EnumSubCommands() }.expect("enumerator");
    assert!(enumerator.cast::<IAgileObject>().is_err(), "the enumerator");
}

/// The scratch folder is long-form even when its base is spelled as an 8.3 short name, as on a
/// runner whose user name is long (`RUNNER~1`); skipped where the volume makes no short names.
#[test]
fn the_scratch_folder_is_long_form_under_a_short_base() {
    use windows::Win32::Storage::FileSystem::GetShortPathNameW;
    let _g = serial();
    init_com();
    let long_base =
        std::env::temp_dir().join(format!("lpk-shell-long-name-{}", protocol::random_hex()));
    std::fs::create_dir_all(&long_base).expect("base");
    let long_w = nul_terminated(&wide(&long_base));
    let mut buf = vec![0u16; 1024];
    // SAFETY: `long_w` is NUL-terminated and `buf` is writable for its length.
    let n = unsafe { GetShortPathNameW(PCWSTR(long_w.as_ptr()), Some(&mut buf)) } as usize;
    let short = PathBuf::from(OsString::from_wide(buf.get(..n).expect("length")));
    if n == 0 || short == long_base {
        eprintln!("skipped: this volume makes no 8.3 names");
        let _ = std::fs::remove_dir_all(&long_base);
        return;
    }
    assert!(short.to_string_lossy().contains('~'), "{short:?}");
    let s = Scratch::under(&short);
    assert!(!s.0.to_string_lossy().contains('~'), "{:?}", s.0);
    // A shell item over a file there reads back exactly the path the test built.
    let f = s.file("a.txt");
    let array = array_of(&names(&[&f]));
    let item = item_at(&array, 0).expect("item");
    assert_eq!(item_path(&item), Some(wide(&f)));
    drop(s);
    let _ = std::fs::remove_dir_all(&long_base);
}
