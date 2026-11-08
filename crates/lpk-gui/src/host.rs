//! The Tauri host (Windows): thin adapters over the command layer (`lpk_gui`). Every command
//! is `async` and moves its work to a blocking thread (a non-`async` command would run on the
//! event loop's thread); long ones register an operation the page can cancel. No command takes
//! a path: grants only (`lpk_gui::grants`).
//!
//! The exit: closing the window or quitting while an operation runs asks first (a native box);
//! then every token is cancelled, and `RunEvent::Exit` closes the sessions and joins the runs
//! before the process ends, so the engine's cleanup (D-87 (2)) completes, also when Windows
//! ends the session (tao ends its loop on `WM_ENDSESSION`).
//!
//! Activation (E5-7): `tauri-plugin-single-instance` is registered first, so a second
//! `lpk-gui.exe` hands its verbs and folder to this one and exits before it touches anything;
//! the recovery of interrupted writes therefore runs in `setup`, in the one instance alone. Each
//! verb's paths are minted as grants (`lpk_gui::activation`) and pushed to the page.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lpk_api::{Class, Event, Monitor};
use lpk_gui::activation::{activate, Activation};
use lpk_gui::app::RecentLists;
use lpk_gui::app::{App, Opened};
use lpk_gui::browse::{SelectAction, Status, ViewInfo, ViewRequest};
use lpk_gui::create::{CreatePlan, CreateRequest, CreateResult, Estimate, NameCheck, TierChoice};
use lpk_gui::extract::{
    Dest, ExtractResult, Plan, Prompter, Request as ExtractRequest, Scope, Verb as ExtractVerb,
};
use lpk_gui::forward::Forwarder;
use lpk_gui::grants::{Grant, GrantKind, Granted};
use lpk_gui::journal::{startup_report, Notice as StartupNotice};
use lpk_gui::maintain::{
    AppendPlan, AppendRequest, AppendResult, CheckResult, Confirm, History, RepairResult,
    RollbackResult, TestResult,
};
use lpk_gui::open::Opening;
use lpk_gui::outbox::Outbox;
use lpk_gui::pick::{NativePicker, Purpose};
use lpk_gui::security::{MAIN_WINDOW, SMOKE_REPORT_ENV};
use lpk_gui::settings::{Settings, Store};
use lpk_gui::shell_setting::{ShellSetting, ShellStatus};
use lpk_gui::{cli, paths, strings, GuiError};
use serde::Serialize;
use tauri::ipc::{Channel, Response};
use tauri::{AppHandle, DragDropEvent, Manager, RunEvent, State, WebviewUrl, WindowEvent};
use zeroize::Zeroizing;

use crate::updates;

/// How long the exit waits for the runs it cancelled.
const EXIT_JOIN: Duration = Duration::from_secs(30);

struct Host {
    app: Arc<App>,
    startup: Option<Activation>,
    startup_error: Option<String>,
    recovered: usize,
    notices: Vec<StartupNotice>,
    settings: Store,
    settings_warning: Option<String>,
    /// The registry and Settings seam (E6-4); `None` when the executable is unknown.
    shell: Option<Arc<ShellSetting>>,
    smoke: Option<PathBuf>,
    /// The page's channels, and the notices pushed before it subscribed (a second instance's
    /// verbs at start-up).
    outbox: Outbox<Channel<Notice>, Notice>,
    /// Second instances' command lines, handled one at a time in arrival order.
    forwarder: Forwarder<(Vec<String>, String)>,
    exit_confirmed: AtomicBool,
}

/// What `startup` tells the page.
#[derive(Serialize)]
struct StartupInfo {
    strings: &'static std::collections::BTreeMap<String, String>,
    /// The command line's verb, its paths as grants.
    activation: Option<Activation>,
    error: Option<String>,
    recovered: usize,
    /// What the recovery must tell the user (D-117): a notice each, shown until dismissed.
    notices: Vec<StartupNotice>,
    /// Why the settings file was not used, if it was not (shown once, as a notice).
    settings_warning: Option<String>,
}

/// Pushed to the page's `subscribe` channel.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Notice {
    /// Files dropped on the window, as grants.
    Dropped { items: Vec<Granted> },
    /// A second instance's verb, its paths as grants.
    Activated { activation: Activation },
    /// A second instance's command line that does not parse.
    Refused { error: String },
    /// The update check found a newer version (E7-7 adds the install).
    Update { version: String },
}

/// A progress event as the page sees it.
#[derive(Clone, Serialize)]
struct ProgressDto {
    stage: &'static str,
    files_done: u64,
    files_total: Option<u64>,
    bytes_in: u64,
    bytes_out: u64,
    done: bool,
}

fn internal(e: impl std::fmt::Display) -> GuiError {
    lpk_api::Error::new(Class::Internal, "Host", e.to_string(), None).into()
}

/// Run `f` on a blocking thread.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, GuiError> + Send + 'static,
) -> Result<T, GuiError> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(internal)?
}

#[tauri::command]
async fn startup(host: State<'_, Host>) -> Result<StartupInfo, GuiError> {
    Ok(StartupInfo {
        strings: strings::table(),
        activation: host.startup.clone(),
        error: host.startup_error.clone(),
        recovered: host.recovered,
        notices: host.notices.clone(),
        settings_warning: host.settings_warning.clone(),
    })
}

/// The settings the page shows (no path among them, `lpk_gui::settings`).
#[tauri::command]
async fn settings(host: State<'_, Host>) -> Result<Settings, GuiError> {
    Ok(host.settings.get())
}

/// Change the settings; a field the page may not write (a path) fails to deserialize.
#[tauri::command]
async fn set_settings(
    host: State<'_, Host>,
    window: tauri::WebviewWindow,
    settings: Settings,
) -> Result<(), GuiError> {
    host.settings.set(settings)?;
    // The title bar follows the Appearance setting too (a failure leaves it as it was).
    let _ = window.set_theme(window_theme(settings.theme));
    Ok(())
}

/// The window's theme for an Appearance setting: `None` follows Windows.
fn window_theme(theme: lpk_gui::settings::Theme) -> Option<tauri::Theme> {
    match theme {
        lpk_gui::settings::Theme::Dark => Some(tauri::Theme::Dark),
        lpk_gui::settings::Theme::Light => Some(tauri::Theme::Light),
        lpk_gui::settings::Theme::System => None,
    }
}

/// The "Open with" setting's state (E6-4): the controls show only when `registered` (this copy
/// is the one the record names). Reads the registry on a blocking thread.
#[tauri::command]
async fn shell_status(host: State<'_, Host>) -> Result<ShellStatus, GuiError> {
    let shell = host.shell.clone();
    blocking(move || {
        Ok(shell.map_or_else(
            || ShellStatus {
                registered: false,
                open_with: false,
                formats: Vec::new(),
            },
            |s| s.status(),
        ))
    })
    .await
}

/// Switch the opt-in "Open with" entries on or off by re-registering in process (refused unless
/// this copy is the registered one); the new status.
#[tauri::command]
async fn set_open_with(host: State<'_, Host>, on: bool) -> Result<ShellStatus, GuiError> {
    let shell = host
        .shell
        .clone()
        .ok_or_else(|| GuiError::usage("this copy of LitePack is not the registered one"))?;
    blocking(move || shell.set_open_with(on)).await
}

/// Open Settings' default-apps page for LitePack (a fixed address; refused unless this copy is
/// the registered one).
#[tauri::command]
async fn open_default_apps(host: State<'_, Host>) -> Result<(), GuiError> {
    let shell = host
        .shell
        .clone()
        .ok_or_else(|| GuiError::usage("this copy of LitePack is not the registered one"))?;
    blocking(move || shell.open_default_apps()).await
}

/// The native password dialog (E5-4), run on the main thread while the blocking task waits on a
/// channel (the event loop is never blocked), owned by the main window and in its theme. The
/// password goes into wiped memory and to the engine; the page learns only the outcome.
struct NativePrompter {
    handle: AppHandle,
    /// The operation's token: a cancel (or the exit, which cancels every token) ends the wait,
    /// and the main-thread closure shows no box once it is set.
    token: lpk_api::CancelToken,
}

impl NativePrompter {
    /// Show the dialog with `texts` on the main thread and wait for its answer.
    fn ask(&self, texts: lpk_gui_sys::PasswordTexts) -> Option<Zeroizing<String>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let owner = owner(&self.handle);
        let dark = self
            .handle
            .get_webview_window(MAIN_WINDOW)
            .and_then(|w| w.theme().ok())
            .is_some_and(|t| t == tauri::Theme::Dark);
        let token = self.token.clone();
        self.handle
            .run_on_main_thread(move || {
                if token.is_cancelled() {
                    let _ = tx.send(Ok(None));
                    return;
                }
                let _ = tx.send(lpk_gui_sys::password_prompt(owner, &texts, dark));
            })
            .ok()?;
        lpk_gui::extract::wait_for_answer(&rx, &self.token)?
            .ok()
            .flatten()
    }
}

impl Prompter for NativePrompter {
    fn password(&self, archive: &str, wrong: bool) -> Option<Zeroizing<String>> {
        let mut texts = strings::password_texts(false);
        texts.title = format!("{} - {archive}", texts.title);
        if wrong {
            texts.prompt = format!("{}\n{}", strings::get("password.wrong"), texts.prompt);
        }
        self.ask(texts)
    }

    fn new_password(&self, _name: &str) -> Option<Zeroizing<String>> {
        let mut texts = strings::password_texts(true);
        texts.prompt = strings::get("create.password.prompt").to_string();
        self.ask(texts)
    }
}

#[tauri::command]
async fn open_archive(
    handle: AppHandle,
    host: State<'_, Host>,
    grant: Grant,
    op: u64,
) -> Result<Opened, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, false)?;
        app.open_with(
            &grant,
            &g.token,
            &NativePrompter {
                handle,
                token: g.token.clone(),
            },
        )
    })
    .await
}

/// A monitor that sends each progress event down `progress` and watches `token`.
fn monitor_of(progress: Channel<ProgressDto>, token: lpk_api::CancelToken) -> Monitor {
    Monitor::new(
        Arc::new(move |e: &Event| {
            let _ = progress.send(ProgressDto {
                stage: e.stage.name(),
                files_done: e.files_done,
                files_total: e.files_total,
                bytes_in: e.bytes_in,
                bytes_out: e.bytes_out,
                done: e.done,
            });
        }),
        token,
    )
}

/// What the extract dialog shows before anything is written: the counts, what exists, the
/// free space (a destination that does not fit is refused here, naming both figures).
#[tauri::command]
async fn extract_plan(
    host: State<'_, Host>,
    session: u64,
    dest: Dest,
    scope: Scope,
    op: u64,
) -> Result<Plan, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, false)?;
        app.extract_plan(session, &dest, scope, &g.token)
    })
    .await
}

/// Extract from an open archive (a heavy operation: queued behind any other).
#[tauri::command]
async fn extract_run(
    handle: AppHandle,
    host: State<'_, Host>,
    session: u64,
    request: ExtractRequest,
    op: u64,
    progress: Channel<ProgressDto>,
) -> Result<ExtractResult, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, true)?;
        let monitor = monitor_of(progress, g.token.clone());
        app.extract_run(
            session,
            &request,
            &NativePrompter {
                handle,
                token: g.token.clone(),
            },
            &monitor,
        )
    })
    .await
}

/// The verbs `--extract-here` (`here`: Smart) and `--extract-to` (`to`: a new folder) for one
/// archive: open, extract everything beside it, close. `memory` is "Allow X" after a refusal.
#[tauri::command]
async fn extract_archive(
    handle: AppHandle,
    host: State<'_, Host>,
    grant: Grant,
    verb: String,
    memory: Option<u64>,
    op: u64,
    progress: Channel<ProgressDto>,
) -> Result<ExtractResult, GuiError> {
    let verb = match verb.as_str() {
        "here" => ExtractVerb::Here,
        "to" => ExtractVerb::To,
        other => return Err(GuiError::usage(format!("verb {other:?}"))),
    };
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, true)?;
        let monitor = monitor_of(progress, g.token.clone());
        app.extract_archive(
            &grant,
            verb,
            memory,
            &NativePrompter {
                handle,
                token: g.token.clone(),
            },
            &monitor,
        )
    })
    .await
}

/// The recent archives and destinations that still exist, as grants minted now.
#[tauri::command]
async fn recents(host: State<'_, Host>) -> Result<RecentLists, GuiError> {
    let app = host.app.clone();
    blocking(move || Ok(app.recents())).await
}

/// Forget the recent archives and destinations.
#[tauri::command]
async fn clear_recents(host: State<'_, Host>) -> Result<(), GuiError> {
    let app = host.app.clone();
    blocking(move || {
        app.clear_recents();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn rows(
    host: State<'_, Host>,
    session: u64,
    first: u32,
    count: u32,
) -> Result<Response, GuiError> {
    let app = host.app.clone();
    let bytes = blocking(move || app.rows(session, first, count)).await?;
    Ok(Response::new(bytes))
}

#[tauri::command]
async fn view(
    host: State<'_, Host>,
    session: u64,
    request: ViewRequest,
    op: u64,
) -> Result<ViewInfo, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, false)?;
        app.view(session, &request, &g.token)
    })
    .await
}

#[tauri::command]
async fn select(
    host: State<'_, Host>,
    session: u64,
    action: SelectAction,
    row: Option<u32>,
    generation: u32,
) -> Result<Status, GuiError> {
    let app = host.app.clone();
    blocking(move || app.select(session, action, row, generation)).await
}

/// Open a file entry: extracted alone into a fresh zoned folder and handed to the shell when
/// its type is allow-listed; otherwise refused before anything is written (the page says the
/// preview shows it, E5-6).
#[tauri::command]
async fn open_entry(
    handle: AppHandle,
    host: State<'_, Host>,
    session: u64,
    row: u32,
    generation: u32,
    op: u64,
) -> Result<Opening, GuiError> {
    let app = host.app.clone();
    let opening = blocking(move || {
        let g = app.ops.begin(op, false)?;
        app.open_entry(
            session,
            row,
            generation,
            &Monitor::new(Arc::new(|_: &Event| {}), g.token.clone()),
        )
    })
    .await?;
    if let Opening::Shell { file, .. } = &opening {
        // The shell call blocks until the handler has the file (`SEE_MASK_NOASYNC`): off the
        // async workers.
        let owner = owner(&handle);
        let file = file.clone();
        blocking(move || lpk_gui_sys::shell_open(owner, &file).map_err(internal)).await?;
    }
    Ok(opening)
}

/// Preview a row (E5-6): one capped read, classified in Rust, returned as one binary message
/// (`lpk_gui::preview::Preview::encode`). A newer preview, `cancel` of `op` or the exit
/// cancels it; it never queues behind a heavy operation.
#[tauri::command]
async fn preview(
    handle: AppHandle,
    host: State<'_, Host>,
    session: u64,
    row: u32,
    generation: u32,
    op: u64,
    unlock: bool,
) -> Result<Response, GuiError> {
    let app = host.app.clone();
    // Registered before the hop to the blocking thread: a `cancel` that arrives early finds it.
    let g = app.ops.begin(op, false)?;
    let bytes = blocking(move || {
        let p = app.preview(
            session,
            row,
            generation,
            &g.token,
            &NativePrompter {
                handle,
                token: g.token.clone(),
            },
            unlock,
        )?;
        Ok(p.encode())
    })
    .await?;
    Ok(Response::new(bytes))
}

/// A native picker (`docs/notes/e5-design.md` section 3): the page names what to pick, the
/// dialog (in Rust) gives the paths, and the page receives a grant per path (none when the
/// user cancels).
#[tauri::command]
async fn pick(
    handle: AppHandle,
    host: State<'_, Host>,
    purpose: Purpose,
) -> Result<Vec<Granted>, GuiError> {
    let app = host.app.clone();
    let picker = NativePicker {
        owner: owner(&handle),
    };
    blocking(move || app.pick(&picker, purpose)).await
}

#[tauri::command]
async fn close_archive(host: State<'_, Host>, session: u64) -> Result<(), GuiError> {
    let app = host.app.clone();
    blocking(move || app.close(session)).await
}

/// The create wizard's defaults and totals for the inputs the page holds grants for.
#[tauri::command]
async fn create_plan(host: State<'_, Host>, inputs: Vec<Grant>) -> Result<CreatePlan, GuiError> {
    let app = host.app.clone();
    blocking(move || app.create_plan(&inputs)).await
}

/// Validate a typed archive name and say whether it exists in the folder.
#[tauri::command]
async fn create_name(
    host: State<'_, Host>,
    folder: Grant,
    name: String,
) -> Result<NameCheck, GuiError> {
    let app = host.app.clone();
    blocking(move || app.create_name(&folder, &name)).await
}

/// A tier's time and size estimate from a calibration on this machine (cancellable: a newer
/// input set supersedes it).
#[tauri::command]
async fn create_calibrate(
    host: State<'_, Host>,
    inputs: Vec<Grant>,
    tier: TierChoice,
    op: u64,
) -> Result<Estimate, GuiError> {
    let app = host.app.clone();
    // Registered before the hop, so that a `cancel` that arrives early finds it.
    let g = app.ops.begin(op, false)?;
    blocking(move || app.calibrate(&inputs, tier, &g.token)).await
}

/// A size estimate with the recovery records of `recovery` percent added (no sampling).
#[tauri::command]
async fn create_scale(bytes: u64, recovery: u8) -> Result<u64, GuiError> {
    Ok(lpk_gui::create::scale_estimate(bytes, recovery))
}

/// Ask for the new archive's password twice (`set`) or forget it; whether one is set. The
/// page never sees the password.
#[tauri::command]
async fn create_password(
    handle: AppHandle,
    host: State<'_, Host>,
    set: bool,
    op: u64,
) -> Result<bool, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        if !set {
            app.clear_password();
            return Ok(false);
        }
        let g = app.ops.begin(op, false)?;
        Ok(app.set_password(&NativePrompter {
            handle,
            token: g.token.clone(),
        }))
    })
    .await
}

/// Create an archive, then test it when asked (a heavy operation: queued behind any other).
#[tauri::command]
async fn create_run(
    host: State<'_, Host>,
    request: CreateRequest,
    op: u64,
    progress: Channel<ProgressDto>,
) -> Result<CreateResult, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, true)?;
        let monitor = monitor_of(progress, g.token.clone());
        app.create_run(&request, &monitor)
    })
    .await
}

/// The native Yes/No box (E5-8's rollback confirmation) on the main thread, owned by the main
/// window; the blocking task waits on a channel. A cancel or the exit counts as No.
struct NativeConfirm {
    handle: AppHandle,
    token: lpk_api::CancelToken,
}

impl Confirm for NativeConfirm {
    fn confirm(&self, title: &str, text: &str) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        let owner = owner(&self.handle);
        let (title, text) = (title.to_string(), text.to_string());
        let token = self.token.clone();
        let sent = self.handle.run_on_main_thread(move || {
            if token.is_cancelled() {
                let _ = tx.send(false);
                return;
            }
            let _ = tx.send(lpk_gui_sys::confirm(owner, &title, &text).unwrap_or(false));
        });
        sent.is_ok() && lpk_gui::extract::wait_for_answer(&rx, &self.token).unwrap_or(false)
    }
}

/// Test an open archive; `unlock` asks for the password first (a heavy operation).
#[tauri::command]
async fn test_run(
    handle: AppHandle,
    host: State<'_, Host>,
    session: u64,
    unlock: bool,
    op: u64,
    progress: Channel<ProgressDto>,
) -> Result<TestResult, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, true)?;
        let monitor = monitor_of(progress, g.token.clone());
        let prompter = NativePrompter {
            handle,
            token: g.token.clone(),
        };
        app.test_run(session, unlock, &prompter, &monitor)
    })
    .await
}

/// The verb `--test` for one archive: open, test, close (a heavy operation).
#[tauri::command]
async fn test_archive(
    handle: AppHandle,
    host: State<'_, Host>,
    grant: Grant,
    unlock: bool,
    op: u64,
    progress: Channel<ProgressDto>,
) -> Result<TestResult, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, true)?;
        let monitor = monitor_of(progress, g.token.clone());
        let prompter = NativePrompter {
            handle,
            token: g.token.clone(),
        };
        app.test_archive(&grant, unlock, &prompter, &monitor)
    })
    .await
}

/// Check the recovery records of an open archive (a heavy operation).
#[tauri::command]
async fn check_run(
    host: State<'_, Host>,
    session: u64,
    op: u64,
    progress: Channel<ProgressDto>,
) -> Result<CheckResult, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, true)?;
        let monitor = monitor_of(progress, g.token.clone());
        app.check_run(session, &monitor)
    })
    .await
}

/// Write a repaired copy of an open archive into `folder` (a folder grant; the archive's own
/// folder when none), then test it (a heavy operation).
#[tauri::command]
async fn repair_run(
    host: State<'_, Host>,
    session: u64,
    folder: Option<Grant>,
    op: u64,
    progress: Channel<ProgressDto>,
) -> Result<RepairResult, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, true)?;
        let monitor = monitor_of(progress, g.token.clone());
        app.repair_run(session, folder.as_ref(), &monitor)
    })
    .await
}

/// The generations of an open archive.
#[tauri::command]
async fn history(host: State<'_, Host>, session: u64, op: u64) -> Result<History, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, false)?;
        app.history(session, &g.token)
    })
    .await
}

/// Open an earlier generation read-only as a new session.
#[tauri::command]
async fn history_browse(
    handle: AppHandle,
    host: State<'_, Host>,
    session: u64,
    generation: u64,
    op: u64,
) -> Result<Opened, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, false)?;
        let prompter = NativePrompter {
            handle,
            token: g.token.clone(),
        };
        app.history_browse(session, generation, &g.token, &prompter)
    })
    .await
}

/// Roll an archive back to `generation` after the native confirmation (a heavy operation: it
/// waits behind a running one, and asks only once it may start).
#[tauri::command]
async fn rollback_run(
    handle: AppHandle,
    host: State<'_, Host>,
    session: u64,
    generation: u64,
    op: u64,
) -> Result<RollbackResult, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, true)?;
        let confirm = NativeConfirm {
            handle: handle.clone(),
            token: g.token.clone(),
        };
        let prompter = NativePrompter {
            handle,
            token: g.token.clone(),
        };
        app.rollback_run(session, generation, &confirm, &prompter, &g.token)
    })
    .await
}

/// What the Update from folder dialog starts with.
#[tauri::command]
async fn append_plan(host: State<'_, Host>, session: u64) -> Result<AppendPlan, GuiError> {
    let app = host.app.clone();
    blocking(move || app.append_plan(session)).await
}

/// Append a folder to an open archive as a new generation (a heavy operation).
#[tauri::command]
async fn append_run(
    handle: AppHandle,
    host: State<'_, Host>,
    session: u64,
    request: AppendRequest,
    op: u64,
    progress: Channel<ProgressDto>,
) -> Result<AppendResult, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, true)?;
        let monitor = monitor_of(progress, g.token.clone());
        let prompter = NativePrompter {
            handle,
            token: g.token.clone(),
        };
        app.append_run(session, &request, &prompter, &monitor)
    })
    .await
}

#[tauri::command]
async fn cancel(host: State<'_, Host>, op: u64) -> Result<bool, GuiError> {
    Ok(host.app.ops.cancel(op))
}

#[tauri::command]
async fn subscribe(host: State<'_, Host>, channel: Channel<Notice>) -> Result<(), GuiError> {
    host.outbox.subscribe(channel, send);
    Ok(())
}

fn send(c: &Channel<Notice>, n: Notice) {
    let _ = c.send(n);
}

/// Send `notice` to the page's channels, or hold it until the page subscribes
/// (`lpk_gui::outbox`: one lock, at most 64 held, then one notice that the rest were dropped).
fn push(host: &Host, notice: Notice) {
    let overflow = || Notice::Refused {
        error: "too many requests arrived before the window was ready; the later ones were dropped"
            .into(),
    };
    host.outbox.push(notice, overflow, send);
}

/// Delete the Explorer menu's response files a parse read (E6-2): only files the shell names,
/// in its folder.
fn delete_shell_files(files: &[std::path::PathBuf]) {
    if let Some(dir) = lpk_gui::shellfile::verbs_dir() {
        for f in files {
            lpk_gui::shellfile::delete(f, &dir);
        }
    }
}

/// The forwarding worker's step: parse a second instance's command line (`argv` with the
/// program name first, relative to `cwd`), mint its grants and push it to the page.
fn forwarded(handle: &AppHandle, argv: Vec<String>, cwd: String) {
    let Some(host) = handle.try_state::<Host>() else {
        return;
    };
    let args = argv.into_iter().skip(1).map(std::ffi::OsString::from);
    let (parsed, files) = cli::parse_with_files(args, Some(std::path::Path::new(&cwd)));
    delete_shell_files(&files);
    match parsed {
        Ok(s) => {
            if let Some(verb) = s.verb {
                let activation = activate(&verb, &host.app.grants);
                host.app.note_activation(&activation);
                push(&host, Notice::Activated { activation });
            }
        }
        Err(error) => push(&host, Notice::Refused { error }),
    }
}

/// A second instance's arguments (the program name first) and folder: its verb reaches the
/// page as grants, and the window comes to the front at once. The parse (which reads `@file`s,
/// perhaps on a slow share) and the minting run on the forwarding worker
/// (`lpk_gui::forward`), one thread in arrival order, never on the event loop's.
fn second_instance(handle: &AppHandle, argv: Vec<String>, cwd: String) {
    if let Some(host) = handle.try_state::<Host>() {
        host.forwarder.send((argv, cwd));
    }
    if let Some(w) = handle.get_webview_window(MAIN_WINDOW) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// The page reports that a listing reached the window. With [`SMOKE_REPORT_ENV`] set (CI's
/// check of the built exe), the host writes what the page reported, then exits; otherwise
/// nothing happens.
#[tauri::command]
async fn listing_shown(
    handle: AppHandle,
    host: State<'_, Host>,
    session: Option<u64>,
    rendered: u32,
    total: u64,
    first: String,
    problems: Vec<String>,
) -> Result<(), GuiError> {
    let Some(path) = host.smoke.clone() else {
        return Ok(());
    };
    let name = match session {
        Some(id) => host.app.session_name(id)?,
        None => String::new(),
    };
    let report = serde_json::json!({
        "archive": name,
        "total": total,
        "rendered": rendered,
        "first": first,
        "problems": problems,
    });
    std::fs::write(&path, report.to_string()).map_err(internal)?;
    handle.exit(0);
    Ok(())
}

fn owner(handle: &AppHandle) -> Option<isize> {
    handle
        .get_webview_window(MAIN_WINDOW)
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as isize)
}

/// Whether the user lets the exit cancel what runs (asked once; nothing running: yes).
fn may_exit(handle: &AppHandle, host: &Host) -> bool {
    if host.exit_confirmed.load(Ordering::SeqCst) || !host.app.ops.busy() {
        return true;
    }
    let yes = lpk_gui_sys::confirm(
        owner(handle),
        strings::get("exit.busy.title"),
        strings::get("exit.busy.text"),
    )
    .unwrap_or(true);
    if yes {
        host.exit_confirmed.store(true, Ordering::SeqCst);
        host.app.ops.cancel_all();
    }
    yes
}

/// The exit code of a portable copy whose `data` folder is unusable.
const PORTABLE_DATA_UNUSABLE: i32 = 1;

/// Tell the user that the portable `data` folder is unusable (the smoke report under CI, a
/// native box otherwise) and give the exit code; never falls back to the profile's folders.
fn refuse_portable(error: &str) -> i32 {
    let text = strings::get("portable.data.text").replace("{error}", error);
    eprintln!("lpk-gui: {text}");
    if let Some(report) = std::env::var_os(SMOKE_REPORT_ENV) {
        if let Err(e) = std::fs::write(&report, paths::data_error_report(&text)) {
            eprintln!("lpk-gui: the smoke report: {e}");
        }
    } else {
        let _ = lpk_gui_sys::alert(None, strings::get("portable.data.title"), &text);
    }
    PORTABLE_DATA_UNUSABLE
}

/// Run the GUI; the process's exit code.
pub fn run() -> i32 {
    // E6-4: `--register-shell` and `--unregister-shell` first of all: no window, no webview, no
    // single-instance plugin (never forwarded to a running instance); they exit with a code.
    let literal: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(code) = lpk_gui::register::handle(&literal) {
        return code;
    }
    // E7-6: a portable copy whose `data` folder cannot be created or written says so and exits,
    // before anything (Tauri, WebView2, the journal) could write elsewhere.
    if let Some(e) = paths::portable_data_error(&paths::dirs()) {
        return refuse_portable(&e);
    }
    let cwd = std::env::current_dir().ok();
    let (parsed, startup_files) =
        cli::parse_with_files(std::env::args_os().skip(1), cwd.as_deref());
    let (startup_verb, startup_error) = match parsed {
        Ok(s) => (s.verb, None),
        Err(e) => {
            eprintln!("lpk-gui: {e}");
            (None, Some(e))
        }
    };
    let app = Arc::new(
        App::new(paths::journal_dir().as_deref())
            .with_open_dir(paths::open_dir())
            .with_recents(lpk_gui::recents::Recents::open(paths::recents_file())),
    );
    let setup_app = app.clone();

    let built = tauri::Builder::default()
        // First: a second instance forwards its verbs here and exits before anything else runs.
        .plugin(tauri_plugin_single_instance::init(|handle, argv, cwd| {
            second_instance(handle, argv, cwd)
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            startup,
            settings,
            set_settings,
            shell_status,
            set_open_with,
            open_default_apps,
            open_archive,
            rows,
            view,
            select,
            open_entry,
            preview,
            pick,
            extract_plan,
            extract_run,
            extract_archive,
            recents,
            clear_recents,
            close_archive,
            create_plan,
            create_name,
            create_calibrate,
            create_scale,
            create_password,
            create_run,
            cancel,
            subscribe,
            listing_shown,
            test_run,
            test_archive,
            check_run,
            repair_run,
            history,
            history_browse,
            rollback_run,
            append_plan,
            append_run,
        ])
        .setup(move |a| {
            let app = setup_app;
            // The previous run's interrupted writes, before anything new starts (this is the one
            // instance); and the folders of entries an earlier run opened, if its process ended.
            let report = startup_report(&app.recover());
            app.sweep_open();
            // This instance is the one that acts on a menu verb: its response files are spent
            // (a second instance exits before here, leaving its file to the first); the others
            // older than a day are swept.
            delete_shell_files(&startup_files);
            lpk_gui::shellfile::sweep();
            let (settings, settings_warning) = Store::open(paths::settings_file());
            if let Some(w) = &settings_warning {
                eprintln!("lpk-gui: {w}");
            }
            let check = settings.get().update_check;
            let theme = settings.get().theme;
            let activation = startup_verb.as_ref().map(|v| activate(v, &app.grants));
            if let Some(a) = &activation {
                app.note_activation(a);
            }
            let worker_handle = a.handle().clone();
            let forwarder = Forwarder::start(move |(argv, cwd): (Vec<String>, String)| {
                forwarded(&worker_handle, argv, cwd)
            })?;
            a.manage(Host {
                app: app.clone(),
                startup: activation,
                startup_error,
                recovered: report.recovered,
                notices: report.notices,
                settings,
                settings_warning,
                shell: ShellSetting::system().map(Arc::new),
                smoke: std::env::var_os(SMOKE_REPORT_ENV).map(PathBuf::from),
                outbox: Outbox::new(),
                forwarder,
                exit_confirmed: AtomicBool::new(false),
            });
            // The update check, once the window is up; off in the settings, or without the
            // release key (E7-7), it requests nothing.
            let handle = a.handle().clone();
            tauri::async_runtime::spawn(async move {
                match updates::check(&handle, check).await {
                    updates::Outcome::Available(version) => {
                        if let Some(host) = handle.try_state::<Host>() {
                            push(&host, Notice::Update { version });
                        }
                    }
                    updates::Outcome::Failed(e) => eprintln!("lpk-gui: the update check: {e}"),
                    _ => {}
                }
            });
            let mut b = tauri::WebviewWindowBuilder::new(
                a,
                MAIN_WINDOW,
                WebviewUrl::App("index.html".into()),
            )
            .title(strings::get("app.title"))
            .inner_size(960.0, 640.0)
            .min_inner_size(480.0, 320.0)
            .theme(window_theme(theme));
            if let Some(d) = paths::webview_dir() {
                b = b.data_directory(d);
            }
            // E5-11: the Edge WebDriver's arguments, only in a `webdriver` build (CI's driver
            // harness; never shipped) and only when the harness asks for them.
            #[cfg(feature = "webdriver")]
            {
                use lpk_gui::security::webdriver;
                let outcome = webdriver::browser_args(
                    std::env::var_os(webdriver::WEBDRIVER_ENV).as_deref(),
                    std::env::var_os(webdriver::WEBVIEW2_ARGS_ENV).as_deref(),
                );
                // The harness cannot read a windowed exe's stderr: the outcome goes to its file.
                if let Some(log) = std::env::var_os(webdriver::WEBDRIVER_LOG_ENV) {
                    let line = match &outcome {
                        Ok(Some(args)) => format!("accepted: {args}\n"),
                        Ok(None) => "not asked\n".to_owned(),
                        Err(e) => format!("refused: {e}\n"),
                    };
                    let _ = std::fs::write(log, line);
                }
                match outcome {
                    Ok(Some(args)) => b = b.additional_browser_args(&args),
                    Ok(None) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            b.build()?;
            Ok(())
        })
        .build(tauri::generate_context!());
    let tauri_app = match built {
        Ok(a) => a,
        Err(e) => {
            eprintln!("lpk-gui: {e}");
            return 1;
        }
    };
    tauri_app.run_return(move |handle, event| match event {
        RunEvent::WindowEvent {
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } => {
            let host = handle.state::<Host>();
            if !may_exit(handle, &host) {
                api.prevent_close();
            }
        }
        RunEvent::WindowEvent {
            event: WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }),
            ..
        } => {
            let host = handle.state::<Host>();
            let items: Vec<Granted> = paths
                .iter()
                .map(|p| {
                    let kind = if p.is_dir() {
                        GrantKind::Input
                    } else {
                        GrantKind::Archive
                    };
                    host.app.grants.mint(kind, p)
                })
                .collect();
            host.app.note_drop(&items);
            push(&host, Notice::Dropped { items });
        }
        RunEvent::ExitRequested { code, api, .. } => {
            let host = handle.state::<Host>();
            if code.is_none() && !may_exit(handle, &host) {
                api.prevent_exit();
            }
        }
        RunEvent::Exit => {
            let joined = app.shutdown(EXIT_JOIN);
            if !joined {
                eprintln!("lpk-gui: an operation did not stop within {EXIT_JOIN:?}");
            }
        }
        _ => {}
    })
}

#[cfg(test)]
mod theme_tests {
    use super::window_theme;
    use lpk_gui::settings::Theme;

    #[test]
    fn the_appearance_setting_maps_to_the_windows_theme() {
        assert_eq!(window_theme(Theme::Dark), Some(tauri::Theme::Dark));
        assert_eq!(window_theme(Theme::Light), Some(tauri::Theme::Light));
        assert_eq!(window_theme(Theme::System), None);
    }
}
