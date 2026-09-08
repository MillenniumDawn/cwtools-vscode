// Process entrypoint for the cwtools-server binary. The module tree, the
// `Backend` dispatch in `server.rs`, and the serve loop in `run` all live in
// the library half so a bench can link against them (#471) and so the Rust
// coverage gate keeps measuring them while excluding this entrypoint (#662).
// `std::env::args` and `std::process::exit` stay here rather than in the
// library, where a process-wide exit has no business being.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("cwtools-server {}", env!("CARGO_PKG_VERSION"));
        eprintln!();
        eprintln!("CWTools language server for Paradox game scripts.");
        eprintln!("Communicates over stdin/stdout using the Language Server Protocol.");
        eprintln!();
        eprintln!("USAGE:");
        eprintln!("    cwtools-server              Start the LSP server (default)");
        eprintln!("    cwtools-server --help       Show this help");
        eprintln!("    cwtools-server --version    Show version");
        std::process::exit(0);
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("cwtools-server {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }

    cwtools_lsp::run();
}
