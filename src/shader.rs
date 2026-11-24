use std::fmt::Write as _;

use glow::HasContext;

use crate::uniforms::Builtin;

pub const DEFAULT_DRAW_VERTEX_COUNT: i32 = 3;

/// Prepended to sources that don't declare a version.
const DEFAULT_VERSION_LINE: &str = "#version 330 core\n";

/// Fullscreen triangle, used when no vertex shader is given.
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

const DRAW_VERTICES_DIRECTIVES: [&str; 2] = ["shady:draw_vertices=", "shady:draw-vertices="];

const PRECISION_QUALIFIERS: [&str; 3] = ["lowp", "mediump", "highp"];

/// Shader sources ready to compile.
pub struct ShaderSet {
    pub vertex: String,
    pub fragment: String,
    pub draw_vertex_count: i32,
    /// Problems that don't stop the shaders from compiling.
    pub warnings: Vec<String>,
}

impl ShaderSet {
    /// Adds the runtime prelude to the user's shaders. Without a vertex shader,
    /// a fullscreen triangle is drawn.
    pub fn assemble(fragment: &str, vertex: Option<&str>) -> Self {
        let fragment_info = SourceInfo::scan(fragment);

        let Some(vertex) = vertex else {
            return Self {
                vertex: VERTEX_SHADER_SOURCE.to_owned(),
                fragment: assemble_stage(fragment, &fragment_info, None, Stage::Fragment),
                draw_vertex_count: DEFAULT_DRAW_VERTEX_COUNT,
                warnings: Vec::new(),
            };
        };

        let mut warnings = Vec::new();
        let vertex_info = SourceInfo::scan(vertex);
        Self {
            vertex: assemble_stage(vertex, &vertex_info, Some(&fragment_info), Stage::Vertex),
            fragment: assemble_stage(
                fragment,
                &fragment_info,
                Some(&vertex_info),
                Stage::Fragment,
            ),
            draw_vertex_count: extract_draw_vertex_count(vertex, &mut warnings),
            warnings,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Stage {
    Vertex,
    Fragment,
}

/// What the prelude needs to know about one shader source.
struct SourceInfo {
    /// Text after `#version`, e.g. "330 core". `None` if there is no `#version`.
    version: Option<String>,
    /// Byte offset just past the `#version` and `#extension` lines, which GLSL
    /// requires before any declarations.
    header_end: usize,
    /// Built-in uniforms the source declares itself, with their declared types.
    declared: Vec<(Builtin, String)>,
    uses_gl_frag_color: bool,
}

impl SourceInfo {
    fn scan(source: &str) -> Self {
        let code = strip_comments(source);
        let (version, header_end) = parse_header(&code);
        Self {
            version,
            header_end,
            declared: declared_builtins(&code),
            uses_gl_frag_color: contains_token(&code, "gl_FragColor"),
        }
    }

    fn declared_type(&self, builtin: Builtin) -> Option<&str> {
        self.declared
            .iter()
            .find(|(b, _)| *b == builtin)
            .map(|(_, ty)| ty.as_str())
    }

    /// Whether the GLSL version allows `out` variables. Before GLSL 1.30 (or
    /// ES 3.00) `gl_FragColor` is a real built-in and needs no shim.
    fn supports_out_variables(&self) -> bool {
        // Without a #version, DEFAULT_VERSION_LINE applies.
        let Some(version) = &self.version else {
            return true;
        };
        let mut parts = version.split_whitespace();
        let Some(Ok(number)) = parts.next().map(str::parse::<u32>) else {
            // Let the compiler report the malformed version.
            return true;
        };
        if parts.next() == Some("es") {
            number >= 300
        } else {
            number >= 130
        }
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether `token` appears in `code` as a whole identifier.
fn contains_token(code: &str, token: &str) -> bool {
    let bytes = code.as_bytes();
    code.match_indices(token).any(|(start, _)| {
        let end = start + token.len();
        let left_ok = start == 0 || !is_ident_char(char::from(bytes[start - 1]));
        let right_ok = end == bytes.len() || !is_ident_char(char::from(bytes[end]));
        left_ok && right_ok
    })
}

/// Blanks out comments, keeping newlines so byte offsets and line numbers
/// still match the original source.
fn strip_comments(source: &str) -> String {
    let mut out = source.as_bytes().to_vec();
    let mut pos = 0;

    while let Some(start) = source[pos..].find('/').map(|i| pos + i) {
        let rest = &source[start..];
        let end = if rest.starts_with("//") {
            rest.find('\n').map_or(source.len(), |i| start + i)
        } else if let Some(body) = rest.strip_prefix("/*") {
            body.find("*/").map_or(source.len(), |i| start + 2 + i + 2)
        } else {
            pos = start + 1;
            continue;
        };
        for byte in &mut out[start..end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        pos = end;
    }

    // Comment delimiters are ASCII, so blanking whole comments keeps the text
    // valid UTF-8.
    String::from_utf8(out).expect("blanking comments keeps UTF-8 valid")
}

/// Splits a preprocessor line such as `#  version 330 core` into
/// `("version", "330 core")`.
fn split_directive(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim().strip_prefix('#')?.trim_start();
    let name_end = rest.find(|c| !is_ident_char(c)).unwrap_or(rest.len());
    Some((&rest[..name_end], rest[name_end..].trim()))
}

/// Finds the `#version` (if any) and where the leading `#version` and
/// `#extension` lines end. `code` must have its comments stripped.
fn parse_header(code: &str) -> (Option<String>, usize) {
    let mut version = None;
    let mut header_end = 0;
    let mut offset = 0;

    for line in code.split_inclusive('\n') {
        offset += line.len();
        match split_directive(line) {
            None if line.trim().is_empty() => continue,
            Some(("version", rest)) if version.is_none() && header_end == 0 => {
                version = Some(rest.to_owned());
            }
            Some(("extension", _)) => {}
            _ => break,
        }
        header_end = offset;
    }

    (version, header_end)
}

/// Finds the built-in uniforms (by name or alias) that the source declares
/// itself, with their declared types. `code` must have its comments stripped.
fn declared_builtins(code: &str) -> Vec<(Builtin, String)> {
    // Preprocessor lines aren't terminated by `;`, so keep them out of the
    // statements.
    let declarations = code
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    let mut declared: Vec<(Builtin, String)> = Vec::new();
    for statement in declarations.split([';', '{', '}']) {
        let mut tokens = statement
            .split(|c| !is_ident_char(c))
            .filter(|token| !token.is_empty());
        if !tokens.any(|token| token == "uniform") {
            continue;
        }
        let mut tokens = tokens.skip_while(|token| PRECISION_QUALIFIERS.contains(token));
        let Some(ty) = tokens.next() else {
            continue;
        };
        for name in tokens {
            let builtin = Builtin::ALL
                .into_iter()
                .find(|b| name == b.name() || name == b.alias());
            if let Some(builtin) = builtin
                && !declared.iter().any(|(b, _)| *b == builtin)
            {
                declared.push((builtin, ty.to_owned()));
            }
        }
    }
    declared
}

fn build_prelude(info: &SourceInfo, other_stage: Option<&SourceInfo>, stage: Stage) -> String {
    let mut out = String::new();

    for builtin in Builtin::ALL {
        if info.declared_type(builtin).is_some() {
            continue;
        }
        // Match the other stage's declaration, or the program won't link.
        let ty = other_stage
            .and_then(|other| other.declared_type(builtin))
            .unwrap_or(builtin.default_type());
        let _ = writeln!(out, "uniform {} {};", ty, builtin.name());
    }

    for builtin in Builtin::ALL {
        let _ = writeln!(out, "#define {} {}", builtin.alias(), builtin.name());
    }

    if stage == Stage::Fragment && info.uses_gl_frag_color && info.supports_out_variables() {
        out.push_str("out vec4 _shady_frag_color;\n");
        out.push_str("#define gl_FragColor _shady_frag_color\n");
    }

    out
}

fn assemble_stage(
    source: &str,
    info: &SourceInfo,
    other_stage: Option<&SourceInfo>,
    stage: Stage,
) -> String {
    let prelude = build_prelude(info, other_stage, stage);
    let (header, body) = source.split_at(info.header_end);

    let mut out =
        String::with_capacity(DEFAULT_VERSION_LINE.len() + source.len() + prelude.len() + 1);
    if info.version.is_none() {
        out.push_str(DEFAULT_VERSION_LINE);
    }
    out.push_str(header);
    if !header.is_empty() && !header.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&prelude);
    out.push_str(body);
    out
}

/// Reads the vertex count from a `// shady:draw_vertices=N` comment.
fn extract_draw_vertex_count(source: &str, warnings: &mut Vec<String>) -> i32 {
    for line in source.lines() {
        let Some(comment) = line.trim().strip_prefix("//") else {
            continue;
        };
        let comment = comment.trim_start();
        let Some(value) = DRAW_VERTICES_DIRECTIVES
            .iter()
            .find_map(|directive| comment.strip_prefix(directive))
        else {
            continue;
        };
        match value.trim().parse::<i32>() {
            Ok(count) if count > 0 => return count,
            _ => warnings.push(format!(
                "ignoring `{}`: the vertex count must be a positive integer",
                line.trim()
            )),
        }
    }

    DEFAULT_DRAW_VERTEX_COUNT
}

fn format_shader_source(source: &str) -> String {
    let mut out = String::new();
    for (i, line) in source.lines().enumerate() {
        let _ = writeln!(out, "{:4}: {}", i + 1, line);
    }
    out
}

fn compile_shader(
    gl: &glow::Context,
    stage: Stage,
    source: &str,
) -> Result<glow::NativeShader, String> {
    let (shader_type, kind) = match stage {
        Stage::Vertex => (glow::VERTEX_SHADER, "vertex"),
        Stage::Fragment => (glow::FRAGMENT_SHADER, "fragment"),
    };

    unsafe {
        let shader = gl
            .create_shader(shader_type)
            .map_err(|err| format!("Failed to create {kind} shader: {err}"))?;
        gl.shader_source(shader, source);
        gl.compile_shader(shader);

        if !gl.get_shader_compile_status(shader) {
            let log = gl.get_shader_info_log(shader);
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

/// Links two compiled shaders. On failure, returns the program info log.
fn link_program(
    gl: &glow::Context,
    vert: glow::NativeShader,
    frag: glow::NativeShader,
) -> Result<glow::NativeProgram, String> {
    unsafe {
        let program = gl
            .create_program()
            .map_err(|err| format!("Failed to create program: {err}"))?;
        gl.attach_shader(program, vert);
        gl.attach_shader(program, frag);
        gl.link_program(program);
        gl.detach_shader(program, vert);
        gl.detach_shader(program, frag);

        if gl.get_program_link_status(program) {
            return Ok(program);
        }
        let log = gl.get_program_info_log(program);
        gl.delete_program(program);
        Err(log)
    }
}

pub fn create_program(
    gl: &glow::Context,
    vert_src: &str,
    frag_src: &str,
) -> Result<glow::NativeProgram, String> {
    let vert = compile_shader(gl, Stage::Vertex, vert_src)?;
    let frag = match compile_shader(gl, Stage::Fragment, frag_src) {
        Ok(frag) => frag,
        Err(err) => {
            unsafe { gl.delete_shader(vert) };
            return Err(err);
        }
    };

    let result = link_program(gl, vert, frag);
    unsafe {
        gl.delete_shader(vert);
        gl.delete_shader(frag);
    }

    result.map_err(|log| {
        let mut message = format!("Error linking program:\n{log}\n");
        // Link errors often involve the vertex shader, but the built-in one
        // isn't worth printing.
        if vert_src != VERTEX_SHADER_SOURCE {
            let _ = write!(
                message,
                "\n--- vertex shader source ---\n{}",
                format_shader_source(vert_src)
            );
        }
        let _ = write!(
            message,
            "\n--- fragment shader source ---\n{}",
            format_shader_source(frag_src)
        );
        message
    })
}

pub fn create_error_program(gl: &glow::Context) -> glow::NativeProgram {
    create_program(gl, VERTEX_SHADER_SOURCE, ERROR_FRAGMENT_SHADER_SOURCE)
        .expect("Built-in error shader must compile")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fragment(source: &str) -> String {
        ShaderSet::assemble(source, None).fragment
    }

    #[test]
    fn adds_version_and_declarations_when_missing() {
        let out = fragment("out vec4 c;\nvoid main() { c = vec4(u_time); }\n");
        assert!(out.starts_with("#version 330 core\nuniform float u_time;\n"));
        assert!(out.contains("uniform vec3 u_resolution;\n"));
        assert!(out.contains("#define iTime u_time\n"));
    }

    #[test]
    fn inserts_prelude_after_version_and_extensions() {
        let out = fragment(
            "// header\n#version 330 core\n\n#extension GL_ARB_gpu_shader5 : enable\nout vec4 c;\n",
        );
        let ext = out.find("#extension").unwrap();
        let uniform = out.find("uniform").unwrap();
        assert!(ext < uniform);
        assert_eq!(out.matches("#version").count(), 1);
    }

    #[test]
    fn finds_version_after_block_comment() {
        let out = fragment("/*\n * shader\n */\n#version 330 core\nout vec4 c;\n");
        assert_eq!(out.matches("#version").count(), 1);
        assert!(out.find("#version").unwrap() < out.find("uniform").unwrap());
    }

    #[test]
    fn ignores_version_mentioned_in_comment() {
        let out = fragment("// needs no #version line\nout vec4 c;\n");
        assert!(out.starts_with("#version 330 core\n"));
    }

    #[test]
    fn puts_prelude_before_conditional_blocks() {
        let out = fragment("#ifdef GL_ES\nprecision mediump float;\n#endif\nout vec4 c;\n");
        assert!(out.find("uniform").unwrap() < out.find("#ifdef").unwrap());
    }

    #[test]
    fn keeps_user_declarations_and_their_types() {
        let source = "uniform vec2 u_resolution;\nuniform mediump vec2 iMouse;\nout vec4 c;\n";
        let out = fragment(source);
        assert!(!out.contains("uniform vec3 u_resolution;"));
        assert!(!out.contains("uniform vec4 u_mouse;"));
        assert!(out.contains("uniform float u_time;"));
    }

    #[test]
    fn other_stage_uses_declared_type() {
        let set = ShaderSet::assemble(
            "uniform vec2 u_resolution;\nout vec4 c;\n",
            Some("void main() { gl_Position = vec4(u_resolution, 0.0, 1.0); }\n"),
        );
        assert!(set.vertex.contains("uniform vec2 u_resolution;"));
    }

    #[test]
    fn declarations_in_comments_do_not_count() {
        // One copy in the comment, one from the prelude.
        let out = fragment("/*\nuniform float u_time;\n*/\nout vec4 c;\n");
        assert_eq!(out.matches("uniform float u_time;").count(), 2);
        let one_line = fragment("out vec4 o;uniform float s;void main(){o=vec4(u_time*s);}\n");
        assert!(one_line.contains("uniform float u_time;"));
    }

    #[test]
    fn frag_color_shim_depends_on_version() {
        assert!(
            fragment("void main() { gl_FragColor = vec4(1.0); }\n").contains("_shady_frag_color")
        );
        assert!(
            !fragment("#version 120\nvoid main() { gl_FragColor = vec4(1.0); }\n")
                .contains("_shady_frag_color")
        );
        assert!(
            fragment("#version 300 es\nvoid main() { gl_FragColor = vec4(1.0); }\n")
                .contains("_shady_frag_color")
        );
        assert!(
            !fragment("out vec4 c;\n/* gl_FragColor */\nvoid main() { c = vec4(1.0); }\n")
                .contains("_shady_frag_color")
        );
    }

    #[test]
    fn parses_draw_vertex_count() {
        let mut warnings = Vec::new();
        assert_eq!(
            extract_draw_vertex_count("// shady:draw_vertices=36\n", &mut warnings),
            36
        );
        assert_eq!(
            extract_draw_vertex_count("//shady:draw-vertices= 6\n", &mut warnings),
            6
        );
        assert_eq!(
            extract_draw_vertex_count("void main() {}\n", &mut warnings),
            3
        );
        assert!(warnings.is_empty());
        assert_eq!(
            extract_draw_vertex_count("// shady:draw_vertices=abc\n", &mut warnings),
            3
        );
        assert_eq!(warnings.len(), 1);
    }
}
