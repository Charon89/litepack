//! `lpk`: create, extract and test `.lpk` archives. Ctrl-C cancels the running command
//! through the engine's token (`ctrlc`): the command cleans up and exits 2, `cancelled`.
#![forbid(unsafe_code)]

fn main() {
    let cancel = lpk_core::CancelToken::new();
    let token = cancel.clone();
    // A handler that cannot be installed leaves Ctrl-C to the default (the process ends).
    let _ = ctrlc::set_handler(move || token.cancel());
    let code = lpk_cli::run_with_cancel(
        std::env::args_os(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
        cancel,
    );
    std::process::exit(code);
}
