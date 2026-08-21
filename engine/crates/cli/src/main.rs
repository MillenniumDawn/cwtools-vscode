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
    // Quiet by default; set RUST_LOG or CWTOOLS_PROFILE to turn on logging /
    // profiling. See PROFILING.md and `cwtools_profiling`.
    cwtools_profiling::init_tracing();
    let cli = cli::Cli::parse();
    run::set_output_style(cli.quiet, cli.no_color);
    commands::dispatch(cli.command);
}
