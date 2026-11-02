//! E5-5's command layer (`docs/notes/e5-design.md` section 7): the plan's defaults, each tier
//! with recovery 5 and 0 verified by the `verify` that `lpk t` runs, a password and the
//! non-listable names, cancel, an existing target, a superseded calibration, the verbs through
//! the same path, "Test when done" on a damaged archive, and the grep that no literal rate
//! stands in the estimate code. The tests assert structure, never a speed or a size.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lpk_api::{CancelToken, Class, CreateSettings, Monitor, OpenOptions, Session};
use lpk_gui::activation::{activate, Handler};
use lpk_gui::app::App;
use lpk_gui::create::{
    CreatePlan, CreateRequest, CreateResult, TestOutcome, TierChoice, MAX_RECOVERY,
    PROPOSED_RECOVERY,
};
use lpk_gui::extract::Prompter;
use lpk_gui::grants::{Grant, GrantKind};
use lpk_gui::{cli, GuiError};
use zeroize::Zeroizing;

struct Fixture {
    dir: tempfile::TempDir,
    app: App,
    inputs: Vec<Grant>,
    folder: Grant,
}

/// A folder `docs` of a few compressible files beside an `out` folder.
fn fixture(files: usize, bytes: usize, random: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("docs");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    for i in 0..files {
        let body: Vec<u8> = if random {
            (0..bytes as u32)
                .map(|x| (x.wrapping_mul(2_654_435_761) >> 13) as u8 ^ i as u8)
                .collect()
        } else {
            format!("file {i}: the quick brown fox\n")
                .repeat(bytes / 28 + 1)
                .into_bytes()
        };
        let place = if i % 2 == 0 { "sub/" } else { "" };
        std::fs::write(src.join(format!("{place}f{i}.txt")), body).unwrap();
    }
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let app = App::new(Some(&dir.path().join("journal")));
    let inputs = vec![app.grants.mint(GrantKind::Input, &src).grant];
    let folder = app.grants.mint(GrantKind::Folder, &out).grant;
    Fixture {
        dir,
        app,
        inputs,
        folder,
    }
}

impl Fixture {
    fn out(&self) -> PathBuf {
        self.dir.path().join("out")
    }

    fn request(&self, name: &str, tier: TierChoice, recovery: u8) -> CreateRequest {
        CreateRequest {
            inputs: self.inputs.clone(),
            folder: self.folder.clone(),
            name: name.to_string(),
            tier,
            recovery,
            encrypt: false,
            encrypt_names: true,
            test: true,
        }
    }
}

/// The same check `lpk t` runs on a finished archive.
fn verify(path: &Path, creds: Option<&lpk_api::Credentials>) -> lpk_api::TestReport {
    let s = Session::open(
        path,
        &OpenOptions {
            credentials: creds,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let r = s.test(&Monitor::default()).unwrap();
    s.close().unwrap();
    r
}

struct Answers(Mutex<Option<String>>);

impl Prompter for Answers {
    fn password(&self, _: &str, _: bool) -> Option<Zeroizing<String>> {
        None
    }
    fn new_password(&self, _: &str) -> Option<Zeroizing<String>> {
        self.0.lock().unwrap().take().map(Zeroizing::new)
    }
}

#[test]
fn the_plan_proposes_the_inputs_folder_name_totals_and_the_defaults() {
    let f = fixture(4, 1000, false);
    let plan: CreatePlan = f.app.create_plan(&f.inputs).unwrap();
    assert_eq!(plan.name, "docs");
    assert_eq!((plan.files, plan.tier), (4, TierChoice::Balanced));
    assert!(plan.bytes > 4 * 900);
    assert!(plan.test && plan.encrypt_names);
    let folder = plan.folder.unwrap();
    assert_eq!(
        f.app
            .grants
            .resolve(&folder.grant, GrantKind::Folder)
            .unwrap(),
        f.dir.path()
    );
    // A file input: its stem; no folder grant is an input grant.
    let file = f
        .app
        .grants
        .mint(GrantKind::Input, &f.dir.path().join("docs/f1.txt"));
    let plan = f.app.create_plan(&[file.grant]).unwrap();
    assert_eq!((plan.name.as_str(), plan.files), ("f1", 1));
    assert!(f.app.create_plan(&[]).is_err());
    assert!(f.app.create_plan(std::slice::from_ref(&f.folder)).is_err());
}

#[test]
fn the_wizard_proposes_recovery_5_and_the_cli_default_stays_0() {
    let f = fixture(2, 500, false);
    let plan = f.app.create_plan(&f.inputs).unwrap();
    assert_eq!(plan.recovery, 5);
    assert_eq!(PROPOSED_RECOVERY, 5);
    assert_eq!(plan.max_recovery, 20);
    assert_eq!(CreateSettings::default().recovery, 0);
    // The field's range: more than the maximum is refused before anything is written.
    let e = f
        .app
        .create_run(
            &f.request("x", TierChoice::Store, MAX_RECOVERY + 1),
            &Monitor::default(),
        )
        .unwrap_err();
    assert!(e.is(Class::Usage), "{e}");
    assert!(!f.out().join("x.lpk").exists());
    // The result says which percent was used.
    for r in [5u8, 0] {
        let res = f
            .app
            .create_run(
                &f.request(&format!("r{r}"), TierChoice::Fast, r),
                &Monitor::default(),
            )
            .unwrap();
        assert_eq!(res.recovery, r);
    }
}

#[test]
fn each_tier_creates_an_archive_that_verifies_with_recovery_5_and_0() {
    let f = fixture(6, 20_000, false);
    for tier in [TierChoice::Fast, TierChoice::Balanced, TierChoice::Store] {
        for recovery in [PROPOSED_RECOVERY, 0] {
            let name = format!("{}-{recovery}", tier.tier().name());
            let r: CreateResult = f
                .app
                .create_run(&f.request(&name, tier, recovery), &Monitor::default())
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let path = f.out().join(format!("{name}.lpk"));
            assert!(path.is_file(), "{name}");
            assert!(!f.out().join(format!("{name}.lpk.partial")).exists());
            assert_eq!(r.name, format!("{name}.lpk"));
            assert!(matches!(r.test, TestOutcome::Passed { .. }), "{:?}", r.test);
            assert_eq!((r.entries >= 6, r.encrypted, r.tier), (true, false, tier));
            assert_eq!(r.bytes_out, std::fs::metadata(&path).unwrap().len());
            assert!(r.bytes_in > 0);
            assert!(
                r.command.starts_with("lpk a ")
                    && r.command.contains(&format!("--recovery {recovery}"))
            );
            assert!(r
                .command
                .contains(&format!("--tier {}", tier.tier().name())));
            let v = verify(&path, None);
            assert!(v.chunks_checked && v.entries >= 6);
            // The result's grant opens the archive in the browser.
            let o = f.app.open(&r.archive.grant, &CancelToken::new()).unwrap();
            assert_eq!(o.name, r.name);
            f.app.close(o.session).unwrap();
        }
    }
    // The output folder joined the recent folders.
    assert_eq!(f.app.recents().destinations.len(), 1);
}

#[test]
fn a_password_hides_the_names_and_opens_with_it() {
    let f = fixture(3, 2000, false);
    let prompt = Answers(Mutex::new(Some("hunter2".to_string())));
    assert!(!f.app.has_password());
    assert!(f.app.set_password(&prompt));
    assert!(f.app.has_password());
    // A second dialog the user cancels leaves the first password in place.
    assert!(f.app.set_password(&Answers(Mutex::new(None))));
    let mut req = f.request("sealed", TierChoice::Store, 0);
    req.encrypt = true;
    let r = f.app.create_run(&req, &Monitor::default()).unwrap();
    assert!(r.encrypted && r.names_encrypted);
    assert!(matches!(r.test, TestOutcome::Passed { .. }), "{:?}", r.test);
    assert!(r.command.contains("--password-prompt") && !r.command.contains("hunter2"));
    assert!(!r.command.contains("--listable"));
    // The password was spent by the run.
    assert!(!f.app.has_password());
    let path = f.out().join("sealed.lpk");
    // Without the password it opens keyless, and the names are not listable.
    let keyless = Session::open(&path, &OpenOptions::default()).unwrap();
    let i = keyless.info();
    assert!(i.encrypted && !i.listable && i.keyless, "{i:?}");
    keyless.close().unwrap();
    let creds = lpk_api::Credentials::password(b"hunter2".to_vec());
    let v = verify(&path, Some(&creds));
    assert!(v.chunks_checked);
    // Names left in clear lists without the password.
    let prompt = Answers(Mutex::new(Some("pw".to_string())));
    assert!(f.app.set_password(&prompt));
    let mut req = f.request("listable", TierChoice::Store, 0);
    (req.encrypt, req.encrypt_names) = (true, false);
    let r = f.app.create_run(&req, &Monitor::default()).unwrap();
    assert!(r.encrypted && !r.names_encrypted && r.command.contains("--listable"));
    let keyless = Session::open(&f.out().join("listable.lpk"), &OpenOptions::default()).unwrap();
    let i = keyless.info();
    assert!(i.encrypted && i.listable, "{i:?}");
    keyless.close().unwrap();
    // `encrypt` without a password set is refused, and clearing forgets one.
    let mut req = f.request("nopw", TierChoice::Store, 0);
    req.encrypt = true;
    assert!(f
        .app
        .create_run(&req, &Monitor::default())
        .unwrap_err()
        .is(Class::Usage));
    assert!(f.app.set_password(&Answers(Mutex::new(Some("x".into())))));
    f.app.clear_password();
    assert!(!f.app.has_password());
    assert!(!f
        .app
        .set_password(&Answers(Mutex::new(Some(String::new())))));
}

#[test]
fn cancel_mid_run_leaves_neither_the_archive_nor_the_partial() {
    let f = fixture(48, 512 * 1024, true);
    let token = CancelToken::new();
    let t2 = token.clone();
    let monitor = Monitor::new(
        Arc::new(move |_: &lpk_api::Event| {
            t2.cancel();
        }),
        token,
    );
    let e = f
        .app
        .create_run(&f.request("big", TierChoice::Fast, 5), &monitor)
        .unwrap_err();
    assert!(e.is(Class::Cancelled), "{e}");
    assert!(!f.out().join("big.lpk").exists());
    assert!(!f.out().join("big.lpk.partial").exists());
    // No journal record is left either.
    let journal = f.app.journal.as_ref().unwrap().dir();
    assert_eq!(std::fs::read_dir(journal).unwrap().count(), 0);
}

#[test]
fn an_existing_target_or_partial_is_refused_before_the_run() {
    let f = fixture(2, 500, false);
    std::fs::write(f.out().join("a.lpk"), b"mine").unwrap();
    std::fs::write(f.out().join("b.lpk.partial"), b"mine").unwrap();
    for name in ["a", "b"] {
        let check = f.app.create_name(&f.folder, name).unwrap();
        assert!(check.exists, "{name}");
        assert_eq!(check.file, format!("{name}.lpk"));
        let e = f
            .app
            .create_run(&f.request(name, TierChoice::Store, 0), &Monitor::default())
            .unwrap_err();
        assert!(e.is(Class::Exists), "{e}");
    }
    assert_eq!(std::fs::read(f.out().join("a.lpk")).unwrap(), b"mine");
    let free = f.app.create_name(&f.folder, "c.lpk").unwrap();
    assert!(!free.exists);
    for bad in ["", "..", "a/b", "con", "x?"] {
        assert!(
            f.app
                .create_name(&f.folder, bad)
                .unwrap_err()
                .is(Class::Usage),
            "{bad:?}"
        );
    }
}

#[test]
fn a_calibration_gives_scaled_estimates_and_a_superseded_one_is_cancelled() {
    let f = fixture(6, 50_000, false);
    for tier in [TierChoice::Fast, TierChoice::Balanced, TierChoice::Store] {
        let e = f
            .app
            .calibrate(&f.inputs, tier, &CancelToken::new())
            .unwrap();
        assert_eq!(e.tier, tier);
        assert!(e.total > 0 && e.sample > 0 && e.sample <= e.total);
        assert!(e.seconds.is_some_and(|s| s >= 0.0) && e.bytes.is_some());
    }
    // A new input set cancels the running calibration through the operation's token.
    let token = f.app.ops.begin(1, false).unwrap();
    assert!(f.app.ops.cancel(1));
    let e = f
        .app
        .calibrate(&f.inputs, TierChoice::Balanced, &token.token)
        .unwrap_err();
    assert!(e.is(Class::Cancelled), "{e}");
    // Mid-run: cancelled while the sample compresses.
    let big = fixture(8, 4 * 1024 * 1024, true);
    let token = CancelToken::new();
    let t2 = token.clone();
    let h = std::thread::spawn({
        let inputs = big.inputs.clone();
        move || {
            big.app
                .calibrate(&inputs, TierChoice::Balanced, &t2)
                .map(|_| ())
        }
    });
    token.cancel();
    if let Err(e) = h.join().unwrap() {
        assert!(e.is(Class::Cancelled), "{e}");
    }
    // Nothing to sample: no figure, no failure.
    let empty = tempfile::tempdir().unwrap();
    let e = f.app.grants.mint(GrantKind::Input, empty.path());
    let est = f
        .app
        .calibrate(&[e.grant], TierChoice::Fast, &CancelToken::new())
        .unwrap();
    assert!(est.seconds.is_none() && est.bytes.is_none());
}

fn act(args: &[&str], cwd: &Path, app: &App) -> lpk_gui::activation::Activation {
    let s = cli::parse(args.iter().map(OsString::from), Some(cwd)).unwrap();
    activate(&s.verb.unwrap(), &app.grants)
}

#[test]
fn the_create_verbs_open_the_wizards_plan_and_run_through_the_same_path() {
    let f = fixture(3, 1000, false);
    let cwd = f.dir.path();
    // `--create <paths>`: the wizard with those inputs.
    let a = act(&["--create", "docs"], cwd, &f.app);
    assert_eq!(a.handler, Handler::CreateWizard);
    assert!(Handler::CreateWizard.implemented());
    let grants: Vec<Grant> = a.items.iter().map(|i| i.grant.clone()).collect();
    let plan = f.app.create_plan(&grants).unwrap();
    assert_eq!((plan.name.as_str(), plan.files), ("docs", 3));
    assert_eq!(plan.recovery, 5);
    // `--create … --name`: created at once with the defaults, through `create_run`.
    let a = act(&["--create", "docs", "--name", "named.lpk"], cwd, &f.app);
    assert_eq!(a.handler, Handler::CreateNamed);
    let req = CreateRequest {
        inputs: a.items.iter().map(|i| i.grant.clone()).collect(),
        folder: a.folder.unwrap().grant,
        name: a.name.unwrap(),
        tier: TierChoice::Balanced,
        recovery: PROPOSED_RECOVERY,
        encrypt: false,
        encrypt_names: true,
        test: true,
    };
    let r = f.app.create_run(&req, &Monitor::default()).unwrap();
    assert_eq!(r.name, "named.lpk");
    assert!(f.dir.path().join("named.lpk").is_file());
    assert!(matches!(r.test, TestOutcome::Passed { .. }));
}

#[test]
fn test_when_done_names_the_failure_of_a_damaged_archive() {
    let f = fixture(6, 30_000, true);
    let flip = AtomicBool::new(false);
    let damage = |p: &Path| {
        let mut b = std::fs::read(p).unwrap();
        let at = b.len() / 2;
        b[at] ^= 0xff;
        std::fs::write(p, b).unwrap();
        flip.store(true, Ordering::SeqCst);
    };
    let r = f
        .app
        .create_run_hooked(
            &f.request("hurt", TierChoice::Store, 0),
            &Monitor::default(),
            Some(&damage),
        )
        .unwrap();
    assert!(flip.load(Ordering::SeqCst));
    match &r.test {
        TestOutcome::Failed { detail } => assert!(!detail.is_empty()),
        other => panic!("{other:?}"),
    }
    // The archive is kept for the user to look at.
    assert!(f.out().join("hurt.lpk").is_file());
    // Off: no test stage.
    let mut req = f.request("untested", TierChoice::Store, 0);
    req.test = false;
    let r = f.app.create_run(&req, &Monitor::default()).unwrap();
    assert_eq!(r.test, TestOutcome::Skipped);
}

/// The estimate code holds no literal rate. Forbidden in `crates/lpk-gui/src` and `apps/gui`
/// (every subfolder; not the tests directory, which holds this file): a name containing
/// `rate`, `ratio`, `speed`, `throughput`, `mbps`, `bytes_per_s` or `per_second`, or a tier's
/// name, followed by `:` or `=` and a numeric literal; a numeric literal multiplying or
/// dividing such a name; and any float or exponent literal (`0.35`, `1.2e8`, `1e6`) next to a
/// `*` or `/` (a conversion constant is spelled as a product of integers, such as
/// `1000 * 1000`). A size unit such as `16 << 20` or the `100` of a percent is not a rate.
#[test]
fn the_estimate_code_holds_no_literal_rate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in [root.join("src"), root.join("../../apps/gui")] {
        collect(&dir, &mut files);
    }
    assert!(files.len() > 20, "the scan found the sources");
    for p in files {
        let text = std::fs::read_to_string(&p).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            assert!(
                !matches_rate_pattern(code, 0)
                    && !matches_rate_pattern(code, 1)
                    && !float_operand(code),
                "{}:{}: a literal rate: {line}",
                p.display(),
                n + 1
            );
        }
    }
}

/// The `.rs`, `.js` and `.html` files under `dir`, subfolders included.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            collect(&p, out);
        } else if matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("rs" | "js" | "html")
        ) {
            out.push(p);
        }
    }
}

/// Whether `t` is, whole, a float or exponent literal: `0.35`, `.35`, `1.2e8`, `1e6`, `1e+8`,
/// `2.5f64` (digits and `_`, an optional fraction, an optional exponent with a sign; at least
/// the fraction or the exponent).
fn is_float(t: &str) -> bool {
    let t = t.trim_end_matches("f64").trim_end_matches("f32");
    let mut it = t.chars().peekable();
    let digits = |it: &mut std::iter::Peekable<std::str::Chars<'_>>| {
        let mut n = 0;
        while it.peek().is_some_and(|c| c.is_ascii_digit() || *c == '_') {
            n += usize::from(it.next() != Some('_'));
        }
        n
    };
    let int = digits(&mut it);
    let mut frac = 0;
    let mut dot = false;
    if it.peek() == Some(&'.') {
        it.next();
        dot = true;
        frac = digits(&mut it);
    }
    let mut exp = false;
    if matches!(it.peek(), Some('e' | 'E')) {
        it.next();
        if matches!(it.peek(), Some('+' | '-')) {
            it.next();
        }
        if digits(&mut it) == 0 {
            return false;
        }
        exp = true;
    }
    it.next().is_none() && (int + frac) > 0 && ((dot && frac > 0) || exp)
}

/// Whether some prefix (`after`) or suffix of `run` is a float literal.
fn has_float(run: &str, after: bool) -> bool {
    let n = run.len();
    (0..=n)
        .filter(|i| run.is_char_boundary(*i))
        .any(|i| is_float(if after { &run[..i] } else { &run[i..] }))
}

/// A float or exponent literal right after or right before a `*` or `/`.
fn float_operand(line: &str) -> bool {
    // Text inside double quotes (a protocol line, a message) is not arithmetic.
    let mut plain = String::new();
    let mut quoted = false;
    for c in line.chars() {
        if c == '"' {
            quoted = !quoted;
        } else if !quoted {
            plain.push(c);
        }
    }
    let line = plain.as_str();
    let word = |c: &char| c.is_ascii_alphanumeric() || matches!(*c, '_' | '.' | '+' | '-');
    for (i, c) in line.char_indices() {
        if c != '*' && c != '/' {
            continue;
        }
        let after: String = line[i + 1..]
            .trim_start()
            .chars()
            .take_while(word)
            .collect();
        let mut before: Vec<char> = line[..i]
            .trim_end()
            .chars()
            .rev()
            .take_while(word)
            .collect();
        before.reverse();
        let before: String = before.into_iter().collect();
        if has_float(&after, true) || has_float(&before, false) {
            return true;
        }
    }
    false
}

const NAMES: [&str; 11] = [
    "rate",
    "ratio",
    "speed",
    "throughput",
    "mbps",
    "bytes_per_s",
    "per_second",
    "fast",
    "balanced",
    "store",
    "_per_second",
];

/// `which` 0: NAME followed by `:` or `=` and a digit; 1: NAME next to `*` or `/` and a digit.
fn matches_rate_pattern(line: &str, which: u8) -> bool {
    let l = line.to_ascii_lowercase();
    let b = l.as_bytes();
    for name in NAMES {
        let mut from = 0;
        while let Some(i) = l[from..].find(name) {
            let start = from + i;
            from = start + name.len();
            // A word's start only (`generation` is not `ratio`); `_` may join words.
            if b[..start].last().is_some_and(u8::is_ascii_alphabetic) {
                continue;
            }
            let mut end = from;
            while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == b'_') {
                end += 1;
            }
            let rest = l[end..].trim_start();
            let digit_after = |s: &str| s.chars().next().is_some_and(|c| c.is_ascii_digit());
            let hit = match which {
                0 => {
                    // `NAME: 3`, `NAME = 3`, `NAME: f64 = 3`; `==` and `=>` are not assignments.
                    rest.strip_prefix([':', '=']).is_some_and(|r| {
                        let r = r.trim_start();
                        if r.starts_with(['=', '>']) {
                            return false;
                        }
                        let ty: String = r
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                            .collect();
                        let after_ty = r[ty.len()..].trim_start();
                        digit_after(r)
                            || (!ty.is_empty()
                                && after_ty.strip_prefix('=').is_some_and(|x| {
                                    !x.starts_with(['=', '>']) && digit_after(x.trim_start())
                                }))
                    })
                }
                _ => {
                    let after = rest
                        .strip_prefix(['*', '/'])
                        .is_some_and(|r| digit_after(r.trim_start()));
                    // A number (a token starting with a digit) before the operator.
                    let before = l[..start]
                        .trim_end()
                        .strip_suffix(['*', '/'])
                        .is_some_and(|r| {
                            let tok: String = r
                                .trim_end()
                                .chars()
                                .rev()
                                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect();
                            tok.starts_with(|c: char| c.is_ascii_digit())
                        });
                    after || before
                }
            };
            if hit {
                return true;
            }
        }
    }
    false
}

#[test]
fn the_rate_pattern_finds_a_literal_and_ignores_sizes() {
    for bad in [
        "let rate = 12.5;",
        "const SPEED: f64 = 40.0;",
        "balanced: 3,",
        "seconds = bytes / fast * 2",
        "ratio = 0.4",
        "bytes_per_second: 100",
        "const s = bytes / 1.2e8;",
        "const mbs = ev.bytes_in / elapsed / 1e6;",
        "let t = total as f64 * 0.35;",
        "x = 2.5 * y",
        "const s = bytes / 1e+8;",
        "const t = total * .35;",
        "let u = n / 2.5e-3;",
    ] {
        assert!(
            matches_rate_pattern(bad, 0) || matches_rate_pattern(bad, 1) || float_operand(bad),
            "{bad}"
        );
    }
    for ok in [
        "pub const SAMPLE_BYTES: u64 = 16 << 20;",
        "tier: TierChoice::Balanced,",
        "if (x == fast) { run(); }",
        "total as f64 / per_second",
        "recovery: PROPOSED_RECOVERY,",
        "const MB = 1000 * 1000;",
        "Math.round((a / b) * 100)",
        "(data * 105 / 100) as u64",
        "seconds < 1.5 && x.toFixed(1)",
        "a / 3 - 1.5",
        "n * 2 + x.y",
    ] {
        assert!(
            !matches_rate_pattern(ok, 0) && !matches_rate_pattern(ok, 1) && !float_operand(ok),
            "{ok}"
        );
    }
}

#[test]
fn errors_stay_gui_errors() {
    // A bad grant kind is a usage error, not a panic.
    let f = fixture(1, 100, false);
    let mut req = f.request("x", TierChoice::Store, 0);
    req.folder = f.inputs[0].clone();
    let e: GuiError = f.app.create_run(&req, &Monitor::default()).unwrap_err();
    assert!(e.is(Class::Usage), "{e}");
}
