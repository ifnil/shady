use std::fmt::Write as _;

use glow::HasContext;

pub const DEFAULT_DRAW_VERTEX_COUNT: i32 = 3;

pub const VERTEX_SHADER_SOURCE: &str = "\
#version 330 core
void main() {
    vec2 pos = vec2(
        float((gl_VertexID & 1) * 4 - 1),
        float((gl_VertexID & 2) * 2 - 1)
    );
    gl_Position = vec4(pos, 0.0, 1.0);
}
";

const SHADER_UNIFORMS: &str = "\
uniform float u_time;
uniform vec3 u_resolution;
uniform vec4 u_mouse;
uniform int u_frame;

#define iTime u_time
#define iResolution u_resolution
#define iMouse u_mouse
#define iFrame u_frame
";

const ERROR_FRAGMENT_SHADER_SOURCE: &str = "\
#version 330 core
out vec4 fragColor;
void main() {
    vec2 cell = floor(gl_FragCoord.xy / 20.0);
    float checker = mod(cell.x + cell.y, 2.0);
    vec3 dark = vec3(0.18, 0.00, 0.18);
    vec3 bright = vec3(0.95, 0.00, 0.95);
    fragColor = vec4(mix(dark, bright, checker), 1.0);
}
";

fn assemble_shader_source(source: &str) -> String {
    if let Some(idx) = source.find("#version") {
        // Insert uniforms after the #version line.
        let line_end = source[idx..]
            .find('\n')
            .map(|i| idx + i + 1)
            .unwrap_or(source.len());
        let mut out = String::with_capacity(source.len() + SHADER_UNIFORMS.len());
        out.push_str(&source[..line_end]);
        out.push_str(SHADER_UNIFORMS);
        out.push_str(&source[line_end..]);
        return out;
    }

    // No #version present: prepend version + uniforms.
    let mut out = String::with_capacity(source.len() + 64 + SHADER_UNIFORMS.len());
    out.push_str("#version 330 core\n");
    out.push_str(SHADER_UNIFORMS);
    out.push_str(source);
    out
}

pub fn assemble_fragment_shader(source: &str) -> String {
    assemble_shader_source(source)
}

pub fn assemble_vertex_shader(source: &str) -> String {
    assemble_shader_source(source)
}

pub fn extract_draw_vertex_count(source: &str) -> i32 {
    const PREFIXES: [&str; 2] = ["// shady:draw_vertices=", "// shady:draw-vertices="];

    for line in source.lines().take(12) {
        let trimmed = line.trim();
        for prefix in PREFIXES {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                if let Ok(count) = rest.trim().parse::<i32>() {
                    if count > 0 {
                        return count;
                    }
                }
            }
        }
    }

    DEFAULT_DRAW_VERTEX_COUNT
}

fn format_shader_source(source: &str) -> String {
    let mut out = String::new();
    for (i, line) in source.lines().enumerate() {
        let _ = writeln!(&mut out, "{:4}: {}", i + 1, line);
    }
    out
}

fn compile_shader(
    gl: &glow::Context,
    shader_type: u32,
    source: &str,
) -> Result<glow::NativeShader, String> {
    unsafe {
        let shader = gl
            .create_shader(shader_type)
            .expect("Failed to create shader");
        gl.shader_source(shader, source);
        gl.compile_shader(shader);

        if !gl.get_shader_compile_status(shader) {
            let log = gl.get_shader_info_log(shader);
            let kind = if shader_type == glow::VERTEX_SHADER {
                "vertex"
            } else {
                "fragment"
            };
            gl.delete_shader(shader);
            return Err(format!(
                "Error compiling {} shader:\n{}\n\n--- shader source ---\n{}",
                kind,
                log,
                format_shader_source(source)
            ));
        }

        Ok(shader)
    }
}

pub fn create_program(
    gl: &glow::Context,
    vert_src: &str,
    frag_src: &str,
) -> Result<glow::NativeProgram, String> {
    let vert = compile_shader(gl, glow::VERTEX_SHADER, vert_src)?;
    let frag = compile_shader(gl, glow::FRAGMENT_SHADER, frag_src)?;

    unsafe {
        let program = match gl.create_program() {
            Ok(program) => program,
            Err(err) => {
                gl.delete_shader(vert);
                gl.delete_shader(frag);
                return Err(format!("Failed to create program: {}", err));
            }
        };

        gl.attach_shader(program, vert);
        gl.attach_shader(program, frag);
        gl.link_program(program);

        if !gl.get_program_link_status(program) {
            let log = gl.get_program_info_log(program);
            gl.detach_shader(program, vert);
            gl.detach_shader(program, frag);
            gl.delete_shader(vert);
            gl.delete_shader(frag);
            gl.delete_program(program);
            return Err(format!(
                "Error linking program:\n{}\n\n--- fragment shader source ---\n{}",
                log,
                format_shader_source(frag_src)
            ));
        }

        gl.detach_shader(program, vert);
        gl.detach_shader(program, frag);
        gl.delete_shader(vert);
        gl.delete_shader(frag);

        Ok(program)
    }
}

pub fn create_error_program(gl: &glow::Context) -> glow::NativeProgram {
    create_program(gl, VERTEX_SHADER_SOURCE, ERROR_FRAGMENT_SHADER_SOURCE)
        .expect("Built-in error shader must compile")
}

pub fn error_fragment_shader_source() -> &'static str {
    ERROR_FRAGMENT_SHADER_SOURCE
}
