//! `lpk`: create, extract and test `.lpk` archives. Ctrl-C during `a`, `append`, `x` or
//! `export` cancels the command through the engine's token (`ctrlc`; installed only for
//! those commands, the others keep the process default): the command cleans up and exits
//! 2, `cancelled`; a second Ctrl-C exits at once with code 2, skipping the cleanup.
#![forbid(unsafe_code)]

fn main() {
    let code = lpk_cli::run_with_cancel(
        std::env::args_os(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
        |token| {
            // A handler that cannot be installed leaves Ctrl-C to the default.
            let _ = ctrlc::set_handler(move || {
                if token.is_cancelled() {
                    std::process::exit(lpk_cli::EXIT_FAILED);
                }
                token.cancel();
            });
        },
    );
    std::process::exit(code);
}
