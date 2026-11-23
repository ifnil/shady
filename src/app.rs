use std::num::NonZero;
use std::time::{Duration, Instant};

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

use crate::files::ShaderFiles;
use crate::shader::{DEFAULT_DRAW_VERTEX_COUNT, ShaderSet, create_error_program, create_program};
use crate::uniforms::{FrameInputs, Uniforms};

/// How often watch mode checks the shader files for changes.
const WATCH_INTERVAL: Duration = Duration::from_millis(200);

pub struct App {
    // GL plumbing
    gl_context: Option<PossiblyCurrentContext>,
    gl_surface: Option<Surface<WindowSurface>>,
    window: Option<Window>,

    // Rendering state
    gl: Option<glow::Context>,
    program: Option<glow::NativeProgram>,
    vao: Option<glow::NativeVertexArray>,
    uniforms: Uniforms,
    draw_vertex_count: i32,

    // Runtime state
    start_time: Instant,
    frame_count: i32,
    mouse_pos: [f32; 4],
    cursor_pos: [f32; 2],
    window_size: [f32; 2],
    mouse_pressed: bool,

    // Shader state
    files: ShaderFiles,
    /// Shaders loaded at startup, compiled once the GL context exists.
    pending_shaders: Option<ShaderSet>,
    watch_enabled: bool,
    last_watch_check: Instant,
    /// The last watch problem printed, so a lasting one is reported once.
    last_watch_error: Option<String>,
}

impl App {
    pub fn new(files: ShaderFiles, shaders: ShaderSet, watch_enabled: bool) -> Self {
        Self {
            gl_context: None,
            gl_surface: None,
            window: None,
            gl: None,
            program: None,
            vao: None,
            uniforms: Uniforms::default(),
            draw_vertex_count: DEFAULT_DRAW_VERTEX_COUNT,
            start_time: Instant::now(),
            frame_count: 0,
            mouse_pos: [0.0; 4],
            cursor_pos: [0.0; 2],
            window_size: [800.0, 600.0],
            mouse_pressed: false,
            files,
            pending_shaders: Some(shaders),
            watch_enabled,
            last_watch_check: Instant::now(),
            last_watch_error: None,
        }
    }

    pub fn run(mut self, event_loop: EventLoop<()>) {
        event_loop.run_app(&mut self).expect("Event loop error");
    }

    /// Compiles `shaders` and switches to them, or to the built-in error
    /// shader if they don't compile. Returns whether they compiled.
    fn use_shaders(&mut self, shaders: ShaderSet) -> bool {
        let gl = self
            .gl
            .as_ref()
            .expect("GL is initialized before shaders are used");

        let (program, draw_vertex_count, compiled) =
            match create_program(gl, &shaders.vertex, &shaders.fragment) {
                Ok(program) => (program, shaders.draw_vertex_count, true),
                Err(err) => {
                    eprintln!("{err}");
                    eprintln!("Using built-in error shader until source compiles.");
                    (create_error_program(gl), DEFAULT_DRAW_VERTEX_COUNT, false)
                }
            };

        if let Some(old_program) = self.program.replace(program) {
            unsafe { gl.delete_program(old_program) };
        }
        self.uniforms = Uniforms::query(gl, program);
        self.draw_vertex_count = draw_vertex_count;
        self.frame_count = 0;
        self.start_time = Instant::now();
        compiled
    }

    fn maybe_reload_shaders(&mut self) {
        if !self.watch_enabled
            || self.gl.is_none()
            || self.last_watch_check.elapsed() < WATCH_INTERVAL
        {
            return;
        }
        self.last_watch_check = Instant::now();

        let loaded = match self.files.changed() {
            Ok(true) => self.files.load().map(Some),
            Ok(false) => Ok(None),
            Err(err) => Err(err),
        };

        let shaders = match loaded {
            Ok(shaders) => {
                self.last_watch_error = None;
                shaders
            }
            Err(err) => {
                // Editors that save by replacing the file can leave it missing
                // for a moment, so don't repeat the same warning every check.
                if self.last_watch_error.as_ref() != Some(&err) {
                    eprintln!("Watch warning: {err}");
                }
                self.last_watch_error = Some(err);
                return;
            }
        };

        if let Some(shaders) = shaders {
            if self.use_shaders(shaders) {
                eprintln!("Reloaded {}", self.files.describe());
            } else {
                eprintln!("Shader reload failed: {}", self.files.describe());
            }
        }
    }

    fn render(&mut self) {
        let (Some(gl), Some(program), Some(vao)) = (&self.gl, self.program, self.vao) else {
            return;
        };

        unsafe {
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.use_program(Some(program));
            gl.bind_vertex_array(Some(vao));
        }

        self.uniforms.upload(
            gl,
            &FrameInputs {
                time: self.start_time.elapsed().as_secs_f32(),
                resolution: self.window_size,
                cursor: self.cursor_pos,
                mouse: self.mouse_pos,
                frame: self.frame_count,
            },
        );

        unsafe { gl.draw_arrays(glow::TRIANGLES, 0, self.draw_vertex_count) };

        if let (Some(surface), Some(context)) = (&self.gl_surface, &self.gl_context)
            && let Err(err) = surface.swap_buffers(context)
        {
            eprintln!("Failed to swap buffers: {err}");
        }

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

        // Build window + GL display. The depth buffer lets custom vertex
        // shaders draw 3D geometry.
        let window_attrs = Window::default_attributes()
            .with_title("shady")
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));

        let template = ConfigTemplateBuilder::new().with_depth_size(24);
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

        // Create GL context. Every shader is GLSL 3.30, so there is no point
        // falling back to GLES.
        let context_attrs = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 3))))
            .build(raw_window_handle);

        let not_current = unsafe { gl_display.create_context(&gl_config, &context_attrs) }
            .unwrap_or_else(|err| {
                eprintln!(
                    "Error: shady needs OpenGL 3.3, but creating a 3.3 context failed: {err}"
                );
                std::process::exit(1);
            });

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

        // Create empty VAO.
        let vao = unsafe { gl.create_vertex_array().expect("Failed to create VAO") };

        unsafe {
            // LEQUAL so shaders that write a depth of exactly 1.0 still draw.
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
        }

        self.gl_context = Some(gl_context);
        self.gl_surface = Some(surface);
        self.window = Some(window);
        self.gl = Some(gl);
        self.vao = Some(vao);
        self.resize_surface(size);

        if let Some(shaders) = self.pending_shaders.take() {
            self.use_shaders(shaders);
        }
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
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    self.mouse_pressed = true;
                    self.mouse_pos = [
                        self.cursor_pos[0],
                        self.cursor_pos[1],
                        self.cursor_pos[0],
                        self.cursor_pos[1],
                    ];
                }
                ElementState::Released => {
                    self.mouse_pressed = false;
                    self.mouse_pos[2] = -self.mouse_pos[2].abs();
                    self.mouse_pos[3] = -self.mouse_pos[3].abs();
                }
            },
            WindowEvent::RedrawRequested => {
                self.render();
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
