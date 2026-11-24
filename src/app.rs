use std::num::NonZero;

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

use crate::files::Watcher;
use crate::renderer::{LoadReport, Renderer};
use crate::shader::ShaderSet;

/// Shows the shader in a window.
pub struct App {
    // GL plumbing. Fields drop in order, and the surface must go before the
    // window it draws to.
    gl_context: Option<PossiblyCurrentContext>,
    gl_surface: Option<Surface<WindowSurface>>,
    window: Option<Window>,
    renderer: Option<Renderer>,

    // Input state
    window_size: [u32; 2],
    cursor_pos: [f32; 2],
    mouse_pos: [f32; 4],
    mouse_pressed: bool,

    // Shader state
    name: String,
    /// Shaders loaded at startup, compiled once the GL context exists.
    pending_shaders: Option<ShaderSet>,
    watcher: Option<Watcher>,
}

impl App {
    pub fn new(name: String, shaders: ShaderSet, watcher: Option<Watcher>) -> Self {
        Self {
            gl_context: None,
            gl_surface: None,
            window: None,
            renderer: None,
            window_size: [800, 600],
            cursor_pos: [0.0; 2],
            mouse_pos: [0.0; 4],
            mouse_pressed: false,
            name,
            pending_shaders: Some(shaders),
            watcher,
        }
    }

    pub fn run(mut self, event_loop: EventLoop<()>) {
        event_loop.run_app(&mut self).expect("Event loop error");
    }

    fn maybe_reload_shaders(&mut self) {
        let (Some(watcher), Some(renderer)) = (&mut self.watcher, &mut self.renderer) else {
            return;
        };

        match watcher.poll() {
            None => {}
            Some(Err(err)) => eprintln!("Watch warning: {err}"),
            Some(Ok(shaders)) => {
                let report = renderer.use_shaders(shaders);
                print_report(&report);
                if report.error.is_none() {
                    eprintln!("Reloaded {}", self.name);
                } else {
                    eprintln!("Shader reload failed: {}", self.name);
                }
            }
        }
    }

    fn render(&mut self) {
        let Some(renderer) = &mut self.renderer else {
            return;
        };
        renderer.draw(self.window_size, self.cursor_pos, self.mouse_pos);

        if let (Some(surface), Some(context)) = (&self.gl_surface, &self.gl_context)
            && let Err(err) = surface.swap_buffers(context)
        {
            eprintln!("Failed to swap buffers: {err}");
        }
    }

    fn resize_surface(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        let width = NonZero::new(size.width).unwrap_or(NonZero::<u32>::MIN);
        let height = NonZero::new(size.height).unwrap_or(NonZero::<u32>::MIN);

        if let (Some(surface), Some(context)) = (&self.gl_surface, &self.gl_context) {
            surface.resize(context, width, height);
        }
        self.window_size = [width.get(), height.get()];
    }
}

fn print_report(report: &LoadReport) {
    for message in report.messages() {
        eprintln!("{message}");
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
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
        let width = NonZero::new(size.width).unwrap_or(NonZero::<u32>::MIN);
        let height = NonZero::new(size.height).unwrap_or(NonZero::<u32>::MIN);
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
        let _ = surface.set_swap_interval(&gl_context, SwapInterval::Wait(NonZero::<u32>::MIN));

        let gl =
            unsafe { glow::Context::from_loader_function_cstr(|s| gl_display.get_proc_address(s)) };
        let mut renderer = Renderer::new(gl);
        if let Some(shaders) = self.pending_shaders.take() {
            print_report(&renderer.use_shaders(shaders));
        }

        self.gl_context = Some(gl_context);
        self.gl_surface = Some(surface);
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.resize_surface(size);
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
                self.cursor_pos = [
                    position.x as f32,
                    self.window_size[1] as f32 - position.y as f32,
                ];
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
