//! syntact CLI binary entry point (thin wrapper over `cli`).

fn main() {
    std::process::exit(cli::run());
}
