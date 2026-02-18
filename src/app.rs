use std::num::NonZero;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use glow::HasContext;
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext, Version,
};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface, SwapInterval, WindowSurface};
use glutin_winit::DisplayBuilder;
use raw_window_handle::HasWindowHandle;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::Window;

use crate::shader::{
    DEFAULT_DRAW_VERTEX_COUNT, VERTEX_SHADER_SOURCE, assemble_fragment_shader,
    assemble_vertex_shader, create_error_program, create_program, error_fragment_shader_source,
    extract_draw_vertex_count,
};

pub struct App {
    // GL plumbing
    gl_config: Option<glutin::config::Config>,
    gl_context: Option<PossiblyCurrentContext>,
    gl_surface: Option<Surface<WindowSurface>>,
    window: Option<Window>,

    // Rendering state
    gl: Option<glow::Context>,
    program: Option<glow::NativeProgram>,
    vao: Option<glow::NativeVertexArray>,

    // Uniform locations
    u_time: Option<glow::NativeUniformLocation>,
    u_resolution: Option<glow::NativeUniformLocation>,
    u_mouse: Option<glow::NativeUniformLocation>,
    u_frame: Option<glow::NativeUniformLocation>,

    // Runtime state
    start_time: Instant,
    frame_count: i32,
    mouse_pos: [f32; 4],
    cursor_pos: [f32; 2],
    window_size: [f32; 2],
    mouse_pressed: bool,

    // Shader state
    fragment_source: String,
    fragment_path: PathBuf,
    vertex_source: String,
    vertex_path: Option<PathBuf>,
    watch_enabled: bool,
    last_fragment_mtime: Option<SystemTime>,
    last_vertex_mtime: Option<SystemTime>,
    last_watch_check: Instant,
    draw_vertex_count: i32,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fragment_source: String,
        fragment_path: PathBuf,
        vertex_source: String,
        vertex_path: Option<PathBuf>,
        watch_enabled: bool,
        last_fragment_mtime: Option<SystemTime>,
        last_vertex_mtime: Option<SystemTime>,
        draw_vertex_count: i32,
    ) -> Self {
        Self {
            gl_config: None,
            gl_context: None,
            gl_surface: None,
            window: None,
            gl: None,
            program: None,
            vao: None,
            u_time: None,
            u_resolution: None,
            u_mouse: None,
            u_frame: None,
            start_time: Instant::now(),
            frame_count: 0,
            mouse_pos: [0.0; 4],
            cursor_pos: [0.0; 2],
            window_size: [800.0, 600.0],
            mouse_pressed: false,
            fragment_source,
            fragment_path,
            vertex_source,
            vertex_path,
            watch_enabled,
            last_fragment_mtime,
            last_vertex_mtime,
            last_watch_check: Instant::now(),
            draw_vertex_count,
        }
    }

    pub fn run(mut self, event_loop: EventLoop<()>) {
        event_loop.run_app(&mut self).expect("Event loop error");
    }

    fn set_program(
        &mut self,
        program: glow::NativeProgram,
        vertex_source: String,
        fragment_source: String,
        draw_vertex_count: i32,
    ) {
        let gl = self.gl.as_ref().unwrap();

        if let Some(old_program) = self.program.replace(program) {
            unsafe {
                gl.delete_program(old_program);
            }
        }

        self.u_time = unsafe { gl.get_uniform_location(program, "u_time") };
        self.u_resolution = unsafe { gl.get_uniform_location(program, "u_resolution") };
        self.u_mouse = unsafe { gl.get_uniform_location(program, "u_mouse") };
        self.u_frame = unsafe { gl.get_uniform_location(program, "u_frame") };
        self.vertex_source = vertex_source;
        self.fragment_source = fragment_source;
        self.draw_vertex_count = draw_vertex_count;
        self.frame_count = 0;
        self.start_time = Instant::now();
    }

    fn reload_program(
        &mut self,
        vertex_source: String,
        fragment_source: String,
        draw_vertex_count: i32,
    ) -> bool {
        let program_result = {
            let gl = self.gl.as_ref().unwrap();
            create_program(gl, &vertex_source, &fragment_source)
        };

        match program_result {
            Ok(program) => {
                self.set_program(program, vertex_source, fragment_source, draw_vertex_count);
                true
            }
            Err(err) => {
                eprintln!("{}", err);
                eprintln!("Using built-in error shader until source compiles.");
                let fallback_program = {
                    let gl = self.gl.as_ref().unwrap();
                    create_error_program(gl)
                };
                self.set_program(
                    fallback_program,
                    VERTEX_SHADER_SOURCE.to_owned(),
                    error_fragment_shader_source().to_owned(),
                    DEFAULT_DRAW_VERTEX_COUNT,
                );
                false
            }
        }
    }

    fn maybe_reload_shaders(&mut self) {
        if !self.watch_enabled {
            return;
        }

        // Avoid metadata checks every frame.
        if self.last_watch_check.elapsed() < Duration::from_millis(200) {
            return;
        }
        self.last_watch_check = Instant::now();

        let fragment_path = self.fragment_path.clone();
        let vertex_path = self.vertex_path.clone();

        let fragment_metadata = match std::fs::metadata(&fragment_path) {
            Ok(metadata) => metadata,
            Err(err) => {
                eprintln!(
                    "Watch warning: failed to stat {}: {}",
                    fragment_path.display(),
                    err
                );
                return;
            }
        };

        let fragment_modified = match fragment_metadata.modified() {
            Ok(modified) => modified,
            Err(err) => {
                eprintln!(
                    "Watch warning: failed to read mtime for {}: {}",
                    fragment_path.display(),
                    err
                );
                return;
            }
        };

        let fragment_changed = self
            .last_fragment_mtime
            .is_none_or(|last| fragment_modified > last);

        let mut vertex_modified: Option<SystemTime> = None;
        let mut vertex_changed = false;

        if let Some(path) = &vertex_path {
            let metadata = match std::fs::metadata(path) {
                Ok(metadata) => metadata,
                Err(err) => {
                    eprintln!("Watch warning: failed to stat {}: {}", path.display(), err);
                    return;
                }
            };

            let modified = match metadata.modified() {
                Ok(modified) => modified,
                Err(err) => {
                    eprintln!(
                        "Watch warning: failed to read mtime for {}: {}",
                        path.display(),
                        err
                    );
                    return;
                }
            };

            vertex_changed = self.last_vertex_mtime.is_none_or(|last| modified > last);
            vertex_modified = Some(modified);
        }

        if !fragment_changed && !vertex_changed {
            return;
        }

        let fragment_raw = match std::fs::read_to_string(&fragment_path) {
            Ok(source) => source,
            Err(err) => {
                eprintln!(
                    "Watch warning: failed to read {}: {}",
                    fragment_path.display(),
                    err
                );
                return;
            }
        };
        let new_fragment_source = assemble_fragment_shader(&fragment_raw);

        let (new_vertex_source, new_draw_vertex_count) = if let Some(path) = &vertex_path {
            let vertex_raw = match std::fs::read_to_string(path) {
                Ok(source) => source,
                Err(err) => {
                    eprintln!("Watch warning: failed to read {}: {}", path.display(), err);
                    return;
                }
            };
            (
                assemble_vertex_shader(&vertex_raw),
                extract_draw_vertex_count(&vertex_raw),
            )
        } else {
            (VERTEX_SHADER_SOURCE.to_owned(), DEFAULT_DRAW_VERTEX_COUNT)
        };

        let compiled = self.reload_program(
            new_vertex_source,
            new_fragment_source,
            new_draw_vertex_count,
        );

        self.last_fragment_mtime = Some(fragment_modified);
        if vertex_path.is_some() {
            self.last_vertex_mtime = vertex_modified;
        }

        if compiled {
            if vertex_changed {
                if let Some(path) = vertex_path {
                    eprintln!(
                        "Reloaded shaders: {} + {}",
                        path.display(),
                        fragment_path.display()
                    );
                }
            } else {
                eprintln!("Reloaded shader: {}", fragment_path.display());
            }
        } else if vertex_changed {
            if let Some(path) = vertex_path {
                eprintln!(
                    "Shader reload failed: {} + {}",
                    path.display(),
                    fragment_path.display()
                );
            }
        } else {
            eprintln!("Shader reload failed: {}", fragment_path.display());
        }
    }

    fn render(&mut self) {
        let gl = self.gl.as_ref().unwrap();
        let program = self.program.unwrap();
        let vao = self.vao.unwrap();

        unsafe {
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);

            gl.use_program(Some(program));
            gl.bind_vertex_array(Some(vao));

            let elapsed = self.start_time.elapsed().as_secs_f32();
            if let Some(ref loc) = self.u_time {
                gl.uniform_1_f32(Some(loc), elapsed);
            }
            if let Some(ref loc) = self.u_resolution {
                gl.uniform_3_f32(Some(loc), self.window_size[0], self.window_size[1], 1.0);
            }
            if let Some(ref loc) = self.u_mouse {
                gl.uniform_4_f32(
                    Some(loc),
                    self.mouse_pos[0],
                    self.mouse_pos[1],
                    self.mouse_pos[2],
                    self.mouse_pos[3],
                );
            }
            if let Some(ref loc) = self.u_frame {
                gl.uniform_1_i32(Some(loc), self.frame_count);
            }

            gl.draw_arrays(glow::TRIANGLES, 0, self.draw_vertex_count);
        }

        self.gl_surface
            .as_ref()
            .unwrap()
            .swap_buffers(self.gl_context.as_ref().unwrap())
            .unwrap();

        self.frame_count += 1;
    }

    fn resize_surface(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        let width = size.width.max(1);
        let height = size.height.max(1);

        if let (Some(surface), Some(context)) = (&self.gl_surface, &self.gl_context) {
            let w = NonZero::new(width).unwrap();
            let h = NonZero::new(height).unwrap();
            surface.resize(context, w, h);
        }

        if let Some(ref gl) = self.gl {
            unsafe {
                gl.viewport(0, 0, width as i32, height as i32);
            }
        }

        self.window_size = [width as f32, height as f32];
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gl.is_some() {
            // Already initialized, just request a redraw.
            if let Some(ref window) = self.window {
                window.request_redraw();
            }
            return;
        }

        // Build window + GL display.
        let window_attrs = Window::default_attributes()
            .with_title("shady")
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));

        let template = ConfigTemplateBuilder::new();
        let display_builder = DisplayBuilder::new().with_window_attributes(Some(window_attrs));

        let (window, gl_config) = display_builder
            .build(event_loop, template, |configs| {
                configs
                    .reduce(|accum, config| {
                        if config.num_samples() > accum.num_samples() {
                            config
                        } else {
                            accum
                        }
                    })
                    .unwrap()
            })
            .expect("Failed to build display");

        let window = window.expect("Failed to create window");
        let raw_window_handle = window.window_handle().ok().map(|h| h.as_raw());
        let gl_display = gl_config.display();

        // Create GL context.
        let context_attrs = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 3))))
            .build(raw_window_handle);
        let fallback_attrs = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(3, 0))))
            .build(raw_window_handle);

        let not_current = unsafe {
            gl_display
                .create_context(&gl_config, &context_attrs)
                .or_else(|_| gl_display.create_context(&gl_config, &fallback_attrs))
                .expect("Failed to create GL context")
        };

        // Create surface.
        let size = window.inner_size();
        let width = NonZero::new(size.width).unwrap_or(NonZero::new(1).unwrap());
        let height = NonZero::new(size.height).unwrap_or(NonZero::new(1).unwrap());
        let surface_attrs = glutin::surface::SurfaceAttributesBuilder::<WindowSurface>::new()
            .build(raw_window_handle.unwrap(), width, height);
        let surface = unsafe {
            gl_display
                .create_window_surface(&gl_config, &surface_attrs)
                .expect("Failed to create surface")
        };

        // Make context current.
        let gl_context = not_current
            .make_current(&surface)
            .expect("Failed to make GL context current");

        // Try to set vsync (non-fatal if it fails).
        let _ =
            surface.set_swap_interval(&gl_context, SwapInterval::Wait(NonZero::new(1).unwrap()));

        // Create glow context.
        let gl =
            unsafe { glow::Context::from_loader_function_cstr(|s| gl_display.get_proc_address(s)) };

        // Compile shader program; if it fails, use fallback error pattern.
        let program = match create_program(&gl, &self.vertex_source, &self.fragment_source) {
            Ok(program) => program,
            Err(err) => {
                eprintln!("{}", err);
                eprintln!("Using built-in error shader until source compiles.");
                self.vertex_source = VERTEX_SHADER_SOURCE.to_owned();
                self.fragment_source = error_fragment_shader_source().to_owned();
                self.draw_vertex_count = DEFAULT_DRAW_VERTEX_COUNT;
                create_error_program(&gl)
            }
        };

        // Create empty VAO.
        let vao = unsafe { gl.create_vertex_array().expect("Failed to create VAO") };

        // Cache uniform locations.
        let u_time = unsafe { gl.get_uniform_location(program, "u_time") };
        let u_resolution = unsafe { gl.get_uniform_location(program, "u_resolution") };
        let u_mouse = unsafe { gl.get_uniform_location(program, "u_mouse") };
        let u_frame = unsafe { gl.get_uniform_location(program, "u_frame") };

        self.gl_config = Some(gl_config);
        self.gl_context = Some(gl_context);
        self.gl_surface = Some(surface);
        self.window = Some(window);
        self.gl = Some(gl);
        self.program = Some(program);
        self.vao = Some(vao);
        self.u_time = u_time;
        self.u_resolution = u_resolution;
        self.u_mouse = u_mouse;
        self.u_frame = u_frame;
        self.resize_surface(size);
        self.start_time = Instant::now();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                self.resize_surface(size);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                let size = self.window.as_ref().map(|window| window.inner_size());
                if let Some(size) = size {
                    self.resize_surface(size);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // Flip Y so (0,0) is bottom-left (Shadertoy convention).
                self.cursor_pos = [position.x as f32, self.window_size[1] - position.y as f32];
                if self.mouse_pressed {
                    self.mouse_pos[0] = self.cursor_pos[0];
                    self.mouse_pos[1] = self.cursor_pos[1];
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    match state {
                        ElementState::Pressed => {
                            self.mouse_pressed = true;
                            self.mouse_pos[0] = self.cursor_pos[0];
                            self.mouse_pos[1] = self.cursor_pos[1];
                            self.mouse_pos[2] = self.cursor_pos[0];
                            self.mouse_pos[3] = self.cursor_pos[1];
                        }
                        ElementState::Released => {
                            self.mouse_pressed = false;
                            self.mouse_pos[2] = -self.mouse_pos[2].abs();
                            self.mouse_pos[3] = -self.mouse_pos[3].abs();
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if self.gl.is_some() {
                    self.render();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        self.maybe_reload_shaders();
        if let Some(ref window) = self.window {
            window.request_redraw();
        }
    }
}
