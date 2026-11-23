mod app;
mod cli;
mod files;
mod shader;
mod uniforms;

use winit::event_loop::EventLoop;

use crate::app::App;
use crate::cli::{CliError, USAGE, parse_args};
use crate::files::ShaderFiles;

fn main() {
    let options = match parse_args() {
        Ok(options) => options,
        Err(CliError::Help) => {
            println!("{USAGE}");
            return;
        }
        Err(CliError::Invalid(message)) => {
            eprintln!("Error: {message}\n\n{USAGE}");
            std::process::exit(1);
        }
    };

    let mut files = ShaderFiles::new(options.fragment_path, options.vertex_path);
    let shaders = files.load().unwrap_or_else(|err| {
        eprintln!("Error: {err}");
        std::process::exit(1);
    });

    let event_loop = EventLoop::builder()
        .build()
        .expect("Failed to create event loop");

    App::new(files, shaders, options.watch_enabled).run(event_loop);
}
