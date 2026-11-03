//! syntact CLI binary entry point (thin wrapper over `cli`).

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    match cli::run(args) {
        Ok(code) => std::process::exit(code),
        Err(message) => {
            eprintln!("syntact: {message}\n\nusage: syntact <check|build|conflicts|conformance> …");
            std::process::exit(cli::USAGE);
        }
    }
}
