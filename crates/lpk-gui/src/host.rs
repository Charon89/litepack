//! The Tauri host (Windows): thin adapters over the command layer (`lpk_gui`). Every command
//! is `async` and moves its work to a blocking thread (a non-`async` command would run on the
//! event loop's thread); long ones register an operation the page can cancel. No command takes
//! a path: grants only (`lpk_gui::grants`).
//!
//! The exit: closing the window or quitting while an operation runs asks first (a native box);
//! then every token is cancelled, and `RunEvent::Exit` closes the sessions and joins the runs
//! before the process ends, so the engine's cleanup (D-87 (2)) completes, also when Windows
//! ends the session (tao ends its loop on `WM_ENDSESSION`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lpk_api::{Class, CreateSettings, Event, Monitor, Tier};
use lpk_gui::app::{App, Opened};
use lpk_gui::browse::{SelectAction, Status, ViewInfo, ViewRequest};
use lpk_gui::grants::{Grant, GrantKind, Granted};
use lpk_gui::open::Opening;
use lpk_gui::security::{MAIN_WINDOW, SMOKE_REPORT_ENV};
use lpk_gui::{cli, paths, strings, GuiError};
use serde::Serialize;
use tauri::ipc::{Channel, Response};
use tauri::{AppHandle, DragDropEvent, Manager, RunEvent, State, WebviewUrl, WindowEvent};

/// How long the exit waits for the runs it cancelled.
const EXIT_JOIN: Duration = Duration::from_secs(30);

struct Host {
    app: Arc<App>,
    startup: Option<Granted>,
    startup_error: Option<String>,
    recovered: usize,
    smoke: Option<PathBuf>,
    subscribers: Mutex<Vec<Channel<Notice>>>,
    exit_confirmed: AtomicBool,
}

/// What `startup` tells the page.
#[derive(Serialize)]
struct StartupInfo {
    strings: &'static std::collections::BTreeMap<String, String>,
    archive: Option<Granted>,
    error: Option<String>,
    recovered: usize,
}

/// Pushed to the page's `subscribe` channel.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Notice {
    /// Files dropped on the window, as grants.
    Dropped { items: Vec<Granted> },
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
        archive: host.startup.clone(),
        error: host.startup_error.clone(),
        recovered: host.recovered,
    })
}

#[tauri::command]
async fn open_archive(host: State<'_, Host>, grant: Grant, op: u64) -> Result<Opened, GuiError> {
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, false)?;
        app.open(&grant, &g.token)
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
) -> Result<Status, GuiError> {
    let app = host.app.clone();
    blocking(move || app.select(session, action, row)).await
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
    op: u64,
) -> Result<Opening, GuiError> {
    let app = host.app.clone();
    let opening = blocking(move || {
        let g = app.ops.begin(op, false)?;
        app.open_entry(
            session,
            row,
            &Monitor::new(Arc::new(|_: &Event| {}), g.token.clone()),
        )
    })
    .await?;
    if let Opening::Shell { file, .. } = &opening {
        lpk_gui_sys::shell_open(owner(&handle), file).map_err(internal)?;
    }
    Ok(opening)
}

#[tauri::command]
async fn close_archive(host: State<'_, Host>, session: u64) -> Result<(), GuiError> {
    let app = host.app.clone();
    blocking(move || app.close(session)).await
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn create_archive(
    host: State<'_, Host>,
    inputs: Vec<Grant>,
    folder: Grant,
    name: String,
    tier: String,
    op: u64,
    progress: Channel<ProgressDto>,
) -> Result<(), GuiError> {
    let tier = match tier.as_str() {
        "fast" => Tier::Fast,
        "balanced" => Tier::Balanced,
        "store" => Tier::Store,
        other => return Err(GuiError::usage(format!("tier {other:?}"))),
    };
    let app = host.app.clone();
    blocking(move || {
        let g = app.ops.begin(op, true)?;
        let monitor = Monitor::new(
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
            g.token.clone(),
        );
        let settings = CreateSettings {
            tier,
            ..CreateSettings::default()
        };
        app.create(&inputs, &folder, &name, &settings, &monitor)
            .map(|_| ())
    })
    .await
}

#[tauri::command]
async fn cancel(host: State<'_, Host>, op: u64) -> Result<bool, GuiError> {
    Ok(host.app.ops.cancel(op))
}

#[tauri::command]
async fn subscribe(host: State<'_, Host>, channel: Channel<Notice>) -> Result<(), GuiError> {
    host.subscribers.lock().map_err(internal)?.push(channel);
    Ok(())
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

/// Run the GUI; the process's exit code.
pub fn run() -> i32 {
    let (startup_path, startup_error) = match cli::parse(std::env::args_os().skip(1)) {
        Ok(s) => (s.open, None),
        Err(e) => {
            eprintln!("lpk-gui: {e}");
            (None, Some(e))
        }
    };
    let app = Arc::new(App::new(paths::journal_dir().as_deref()).with_open_dir(paths::open_dir()));
    // The previous run's interrupted writes, before anything new starts.
    let recovered = app.recover().len();
    let opened_at_start = startup_path.map(|p| app.grants.mint(GrantKind::Archive, &p));
    let host = Host {
        app: app.clone(),
        startup: opened_at_start,
        startup_error,
        recovered,
        smoke: std::env::var_os(SMOKE_REPORT_ENV).map(PathBuf::from),
        subscribers: Mutex::new(Vec::new()),
        exit_confirmed: AtomicBool::new(false),
    };

    let built = tauri::Builder::default()
        .manage(host)
        .invoke_handler(tauri::generate_handler![
            startup,
            open_archive,
            rows,
            view,
            select,
            open_entry,
            close_archive,
            create_archive,
            cancel,
            subscribe,
            listing_shown,
        ])
        .setup(|a| {
            let mut b = tauri::WebviewWindowBuilder::new(
                a,
                MAIN_WINDOW,
                WebviewUrl::App("index.html".into()),
            )
            .title(strings::get("app.title"))
            .inner_size(960.0, 640.0)
            .min_inner_size(480.0, 320.0);
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
            let subs: Vec<Channel<Notice>> = match host.subscribers.lock() {
                Ok(s) => s.clone(),
                Err(_) => Vec::new(),
            };
            for s in subs {
                let _ = s.send(Notice::Dropped {
                    items: items.clone(),
                });
            }
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
