use std::num::NonZero;
use std::time::Instant;

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

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const VERTEX_SHADER_SOURCE: &str = "\
#version 330 core
void main() {
    vec2 pos = vec2(
        float((gl_VertexID & 1) * 4 - 1),
        float((gl_VertexID & 2) * 2 - 1)
    );
    gl_Position = vec4(pos, 0.0, 1.0);
}
";

const SHADERTOY_PREFIX: &str = "\
#version 330 core
uniform float iTime;
uniform vec3 iResolution;
uniform vec4 iMouse;
uniform int iFrame;

out vec4 _fragColor;
";

const SHADERTOY_SUFFIX: &str = "
void main() {
    mainImage(_fragColor, gl_FragCoord.xy);
}
";

const RAW_UNIFORMS: &str = "\
uniform float iTime;
uniform vec3 iResolution;
uniform vec4 iMouse;
uniform int iFrame;
";

// ---------------------------------------------------------------------------
// Shader assembly
// ---------------------------------------------------------------------------

fn assemble_fragment_shader(source: &str) -> String {
    if source.contains("mainImage") {
        // Shadertoy-style: wrap with prefix + suffix
        let mut out = String::with_capacity(
            SHADERTOY_PREFIX.len() + source.len() + SHADERTOY_SUFFIX.len(),
        );
        out.push_str(SHADERTOY_PREFIX);
        out.push_str(source);
        out.push_str(SHADERTOY_SUFFIX);
        out
    } else if let Some(idx) = source.find("#version") {
        // Raw shader with #version — insert uniforms after the #version line
        let line_end = source[idx..].find('\n').map(|i| idx + i + 1).unwrap_or(source.len());
        let mut out = String::with_capacity(source.len() + RAW_UNIFORMS.len());
        out.push_str(&source[..line_end]);
        out.push_str(RAW_UNIFORMS);
        out.push_str(&source[line_end..]);
        out
    } else {
        // Raw shader without #version — prepend version + uniforms
        let mut out = String::with_capacity(source.len() + 64 + RAW_UNIFORMS.len());
        out.push_str("#version 330 core\n");
        out.push_str(RAW_UNIFORMS);
        out.push_str(source);
        out
    }
}

// ---------------------------------------------------------------------------
// Shader compile / link helpers
// ---------------------------------------------------------------------------

fn compile_shader(gl: &glow::Context, shader_type: u32, source: &str) -> glow::NativeShader {
    unsafe {
        let shader = gl.create_shader(shader_type).expect("Failed to create shader");
        gl.shader_source(shader, source);
        gl.compile_shader(shader);

        if !gl.get_shader_compile_status(shader) {
            let log = gl.get_shader_info_log(shader);
            let kind = if shader_type == glow::VERTEX_SHADER {
                "vertex"
            } else {
                "fragment"
            };
            eprintln!("Error compiling {} shader:\n{}", kind, log);
            eprintln!("\n--- shader source ---");
            for (i, line) in source.lines().enumerate() {
                eprintln!("{:4}: {}", i + 1, line);
            }
            std::process::exit(1);
        }

        shader
    }
}

fn create_program(gl: &glow::Context, vert_src: &str, frag_src: &str) -> glow::NativeProgram {
    let vert = compile_shader(gl, glow::VERTEX_SHADER, vert_src);
    let frag = compile_shader(gl, glow::FRAGMENT_SHADER, frag_src);

    unsafe {
        let program = gl.create_program().expect("Failed to create program");
        gl.attach_shader(program, vert);
        gl.attach_shader(program, frag);
        gl.link_program(program);

        if !gl.get_program_link_status(program) {
            let log = gl.get_program_info_log(program);
            eprintln!("Error linking program:\n{}", log);
            std::process::exit(1);
        }

        gl.detach_shader(program, vert);
        gl.detach_shader(program, frag);
        gl.delete_shader(vert);
        gl.delete_shader(frag);

        program
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct App {
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

    // Shader source
    frag_source: String,
}

impl App {
    fn new(frag_source: String) -> Self {
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
            frag_source,
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

            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }

        self.gl_surface
            .as_ref()
            .unwrap()
            .swap_buffers(self.gl_context.as_ref().unwrap())
            .unwrap();

        self.frame_count += 1;
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gl.is_some() {
            // Already initialized — just request a redraw
            if let Some(ref window) = self.window {
                window.request_redraw();
            }
            return;
        }

        // Build window + GL display
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

        // Create GL context
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

        // Create surface
        let size = window.inner_size();
        let width = NonZero::new(size.width).unwrap_or(NonZero::new(1).unwrap());
        let height = NonZero::new(size.height).unwrap_or(NonZero::new(1).unwrap());
        let surface_attrs =
            glutin::surface::SurfaceAttributesBuilder::<WindowSurface>::new().build(
                raw_window_handle.unwrap(),
                width,
                height,
            );
        let surface = unsafe {
            gl_display
                .create_window_surface(&gl_config, &surface_attrs)
                .expect("Failed to create surface")
        };

        // Make context current
        let gl_context = not_current
            .make_current(&surface)
            .expect("Failed to make GL context current");

        // Try to set vsync (non-fatal if it fails)
        let _ = surface.set_swap_interval(&gl_context, SwapInterval::Wait(NonZero::new(1).unwrap()));

        // Create glow context
        let gl = unsafe {
            glow::Context::from_loader_function_cstr(|s| gl_display.get_proc_address(s))
        };

        // Compile shader program
        let program = create_program(&gl, VERTEX_SHADER_SOURCE, &self.frag_source);

        // Create empty VAO
        let vao = unsafe { gl.create_vertex_array().expect("Failed to create VAO") };

        // Cache uniform locations
        let u_time = unsafe { gl.get_uniform_location(program, "iTime") };
        let u_resolution = unsafe { gl.get_uniform_location(program, "iResolution") };
        let u_mouse = unsafe { gl.get_uniform_location(program, "iMouse") };
        let u_frame = unsafe { gl.get_uniform_location(program, "iFrame") };

        self.window_size = [size.width as f32, size.height as f32];
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
                if let (Some(surface), Some(context)) =
                    (&self.gl_surface, &self.gl_context)
                {
                    let w = NonZero::new(size.width).unwrap_or(NonZero::new(1).unwrap());
                    let h = NonZero::new(size.height).unwrap_or(NonZero::new(1).unwrap());
                    surface.resize(context, w, h);
                    if let Some(ref gl) = self.gl {
                        unsafe {
                            gl.viewport(0, 0, size.width as i32, size.height as i32);
                        }
                    }
                    self.window_size = [size.width as f32, size.height as f32];
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // Flip Y so (0,0) is bottom-left (Shadertoy convention)
                self.cursor_pos = [
                    position.x as f32,
                    self.window_size[1] - position.y as f32,
                ];
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
        if let Some(ref window) = self.window {
            window.request_redraw();
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: shady <shader.glsl>");
        std::process::exit(1);
    }

    let shader_path = &args[1];
    let source = std::fs::read_to_string(shader_path).unwrap_or_else(|e| {
        eprintln!("Error reading {}: {}", shader_path, e);
        std::process::exit(1);
    });

    let frag_source = assemble_fragment_shader(&source);

    let event_loop = EventLoop::builder()
        .build()
        .expect("Failed to create event loop");

    let mut app = App::new(frag_source);
    event_loop.run_app(&mut app).expect("Event loop error");
}
