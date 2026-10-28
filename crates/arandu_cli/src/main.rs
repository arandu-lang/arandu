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
mod output;
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
        // A daemon build renders in the style of the client that asked for it.
        let style = output::style();
        let use_color = style.map_or(use_color, |style| style.color);
        let chars = miette::ThemeCharacters {
            ltop: '┌',
            mtop: '┬',
            rtop: '┐',
            lbot: '└',
            mbot: '┴',
            rbot: '┘',
            lcross: '├',
            rcross: '┤',
            ..miette::ThemeCharacters::unicode()
        };
        let theme = miette::GraphicalTheme {
            characters: chars,
            styles: if use_color {
                miette::ThemeStyles::ansi()
            } else {
                miette::ThemeStyles::none()
            },
        };
        let mut options = miette::MietteHandlerOpts::new()
            .color(use_color)
            .terminal_links(use_color)
            .graphical_theme(theme)
            .context_lines(2)
            .tab_width(4);
        if let Some(width) = style.and_then(|style| style.width) {
            options = options.width(width);
        }
        Box::new(options.build())
    }));

    let result = commands::run(raw_args);
    pipeline::finish(result);
}
