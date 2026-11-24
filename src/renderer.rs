use std::time::Instant;

use glow::HasContext;

use crate::shader::{DEFAULT_DRAW_VERTEX_COUNT, ShaderSet, create_error_program, create_program};
use crate::uniforms::{FrameInputs, Uniforms};

/// The outcome of switching to new shaders.
pub struct LoadReport {
    /// Compile or link error. The built-in error shader is drawn instead.
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

impl LoadReport {
    /// The report as lines for stderr.
    pub fn messages(&self) -> Vec<String> {
        let mut messages: Vec<_> = self
            .warnings
            .iter()
            .map(|warning| format!("Warning: {warning}"))
            .collect();
        if let Some(error) = &self.error {
            messages.push(error.clone());
            messages.push("Using built-in error shader until source compiles.".to_owned());
        }
        messages
    }
}

/// Draws the current shader program and owns the GL objects it needs.
pub struct Renderer {
    gl: glow::Context,
    vao: glow::NativeVertexArray,
    program: glow::NativeProgram,
    uniforms: Uniforms,
    draw_vertex_count: i32,
    start_time: Instant,
    frame_count: i32,
}

impl Renderer {
    /// Sets up GL state. Draws the built-in error shader until `use_shaders`
    /// is called.
    pub fn new(gl: glow::Context) -> Self {
        let vao = unsafe { gl.create_vertex_array() }.expect("Failed to create VAO");
        unsafe {
            // Depth testing lets custom vertex shaders draw 3D geometry. LEQUAL
            // so shaders that write a depth of exactly 1.0 still draw.
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
        }
        let program = create_error_program(&gl);

        Self {
            gl,
            vao,
            program,
            uniforms: Uniforms::default(),
            draw_vertex_count: DEFAULT_DRAW_VERTEX_COUNT,
            start_time: Instant::now(),
            frame_count: 0,
        }
    }

    pub fn gl(&self) -> &glow::Context {
        &self.gl
    }

    /// Compiles `shaders` and switches to them, or to the built-in error
    /// shader if they don't compile.
    pub fn use_shaders(&mut self, shaders: ShaderSet) -> LoadReport {
        let mut warnings = shaders.warnings;
        let (program, draw_vertex_count, error) =
            match create_program(&self.gl, &shaders.vertex, &shaders.fragment) {
                Ok(program) => (program, shaders.draw_vertex_count, None),
                Err(err) => (
                    create_error_program(&self.gl),
                    DEFAULT_DRAW_VERTEX_COUNT,
                    Some(err),
                ),
            };

        let old_program = std::mem::replace(&mut self.program, program);
        unsafe { self.gl.delete_program(old_program) };
        self.uniforms = Uniforms::query(&self.gl, program, &mut warnings);
        self.draw_vertex_count = draw_vertex_count;
        self.frame_count = 0;
        self.start_time = Instant::now();

        LoadReport { error, warnings }
    }

    /// Draws one frame into the bound framebuffer, which is `size` pixels.
    pub fn draw(&mut self, size: [u32; 2], cursor: [f32; 2], mouse: [f32; 4]) {
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, size[0] as i32, size[1] as i32);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
        }

        self.uniforms.upload(
            gl,
            &FrameInputs {
                time: self.start_time.elapsed().as_secs_f32(),
                resolution: [size[0] as f32, size[1] as f32],
                cursor,
                mouse,
                frame: self.frame_count,
            },
        );

        unsafe { gl.draw_arrays(glow::TRIANGLES, 0, self.draw_vertex_count) };
        self.frame_count += 1;
    }
}
