//! Arandu Compiler Command-Line Interface (CLI).
//!
//! Thin process adapter delegating command parsing, pipeline execution,
//! and process status code resolution to modular subsystems.

mod args;
mod artifact;
mod cgu;
mod cli_error;
mod commands;
mod incremental;
mod linker;
mod linker_elf;
mod manifest_io;
mod pipeline;
mod project;
mod test_runner;
mod wasm_opt;
mod watch;

use std::env;

fn main() {
    let raw_args: Vec<String> = env::args().collect();
    let color_choice = args::detect_color_choice(&raw_args);
    let use_color = color_choice.should_color_stderr();

    let _ = miette::set_hook(Box::new(move |_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .color(use_color)
                .terminal_links(use_color)
                .unicode(true)
                .context_lines(2)
                .tab_width(4)
                .build(),
        )
    }));

    let result = commands::run(raw_args);
    pipeline::finish(result);
}
