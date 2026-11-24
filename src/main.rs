mod app;
mod cli;
mod files;
mod renderer;
mod shader;
#[cfg(all(unix, not(target_vendor = "apple")))]
mod terminal;
mod uniforms;

use winit::event_loop::EventLoop;

use crate::app::App;
use crate::cli::{CliError, USAGE, parse_args};
use crate::files::{ShaderFiles, Watcher};
use crate::shader::ShaderSet;

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
    let name = files.describe();
    let watcher = options.watch_enabled.then(|| Watcher::new(files));

    if options.kitty {
        if let Err(err) = run_in_terminal(name, shaders, watcher) {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
        return;
    }

    let event_loop = EventLoop::builder()
        .build()
        .expect("Failed to create event loop");
    App::new(name, shaders, watcher).run(event_loop);
}

#[cfg(all(unix, not(target_vendor = "apple")))]
fn run_in_terminal(
    name: String,
    shaders: ShaderSet,
    watcher: Option<Watcher>,
) -> Result<(), String> {
    terminal::run(name, shaders, watcher)
}

#[cfg(not(all(unix, not(target_vendor = "apple"))))]
fn run_in_terminal(_: String, _: ShaderSet, _: Option<Watcher>) -> Result<(), String> {
    Err("-k renders with EGL, which is only available on Linux and the BSDs".to_owned())
}
