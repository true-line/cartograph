//! Explicit local stdio relay; no application startup, stores or read handlers.

#[cfg(unix)]
mod unix;

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    let result = unix::run();
    #[cfg(not(unix))]
    let result: Result<(), &str> = Err("local relay unsupported on this platform");
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            std::process::ExitCode::FAILURE
        }
    }
}
