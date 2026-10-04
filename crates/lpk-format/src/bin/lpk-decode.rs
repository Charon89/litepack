//! `lpk-decode`: list, verify, extract and describe `.lpk` archives.
#![forbid(unsafe_code)]

fn main() {
    let code = lpk_format::cli::run(
        std::env::args_os(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    std::process::exit(code);
}
