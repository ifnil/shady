mod app;
mod cli;
mod shader;

use winit::event_loop::EventLoop;

use crate::app::App;
use crate::cli::parse_args;
use crate::shader::{
    DEFAULT_DRAW_VERTEX_COUNT, VERTEX_SHADER_SOURCE, assemble_fragment_shader,
    assemble_vertex_shader, extract_draw_vertex_count,
};

fn main() {
    let options = parse_args().unwrap_or_else(|err| {
        eprintln!("{}", err);
        std::process::exit(1);
    });

    let fragment_raw = std::fs::read_to_string(&options.fragment_path).unwrap_or_else(|e| {
        eprintln!("Error reading {}: {}", options.fragment_path.display(), e);
        std::process::exit(1);
    });
    let fragment_source = assemble_fragment_shader(&fragment_raw);
    let fragment_mtime = std::fs::metadata(&options.fragment_path)
        .and_then(|metadata| metadata.modified())
        .ok();

    let (vertex_source, vertex_mtime, draw_vertex_count) = if let Some(path) = &options.vertex_path
    {
        let vertex_raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("Error reading {}: {}", path.display(), e);
            std::process::exit(1);
        });
        let vertex_source = assemble_vertex_shader(&vertex_raw);
        let vertex_mtime = std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok();
        let draw_vertex_count = extract_draw_vertex_count(&vertex_raw);
        (vertex_source, vertex_mtime, draw_vertex_count)
    } else {
        (
            VERTEX_SHADER_SOURCE.to_owned(),
            None,
            DEFAULT_DRAW_VERTEX_COUNT,
        )
    };

    let event_loop = EventLoop::builder()
        .build()
        .expect("Failed to create event loop");

    let app = App::new(
        fragment_source,
        options.fragment_path,
        vertex_source,
        options.vertex_path,
        options.watch_enabled,
        fragment_mtime,
        vertex_mtime,
        draw_vertex_count,
    );
    app.run(event_loop);
}
