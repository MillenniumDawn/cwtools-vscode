use clap::Parser;

mod cli;
mod codes;
mod commands;
mod config;
mod diag;
mod report;
mod run;
mod scope;

fn main() {
    #[cfg(unix)]
    // SAFETY: restoring SIGPIPE only changes this process's signal disposition.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // Quiet by default; set RUST_LOG or CWTOOLS_PROFILE to turn on logging /
    // profiling. See PROFILING.md and `cwtools_profiling`.
    cwtools_profiling::init_tracing();
    let cli = cli::Cli::parse();
    run::set_output_style(cli.quiet, cli.no_color);
    commands::dispatch(cli.command);
}
