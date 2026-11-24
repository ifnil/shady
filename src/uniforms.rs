use glow::HasContext;

/// Uniforms the runtime sets every frame. Shaders can refer to each one by its
/// name or by its Shadertoy-style alias.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Time,
    Resolution,
    Mouse,
    Frame,
}

impl Builtin {
    pub const ALL: [Builtin; 4] = [
        Builtin::Time,
        Builtin::Resolution,
        Builtin::Mouse,
        Builtin::Frame,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Builtin::Time => "u_time",
            Builtin::Resolution => "u_resolution",
            Builtin::Mouse => "u_mouse",
            Builtin::Frame => "u_frame",
        }
    }

    pub fn alias(self) -> &'static str {
        match self {
            Builtin::Time => "iTime",
            Builtin::Resolution => "iResolution",
            Builtin::Mouse => "iMouse",
            Builtin::Frame => "iFrame",
        }
    }

    /// Type the prelude declares when no shader stage declares the uniform.
    pub fn default_type(self) -> &'static str {
        match self {
            Builtin::Time => "float",
            Builtin::Resolution => "vec3",
            Builtin::Mouse => "vec4",
            Builtin::Frame => "int",
        }
    }

    /// GL types the runtime knows how to set for this uniform.
    fn accepted_types(self) -> &'static [u32] {
        match self {
            Builtin::Time => &[glow::FLOAT],
            Builtin::Resolution => &[glow::FLOAT_VEC2, glow::FLOAT_VEC3],
            Builtin::Mouse => &[glow::FLOAT_VEC2, glow::FLOAT_VEC4],
            Builtin::Frame => &[glow::INT, glow::FLOAT],
        }
    }
}

/// Values for the built-in uniforms in one frame.
pub struct FrameInputs {
    pub time: f32,
    pub resolution: [f32; 2],
    /// Cursor position with a bottom-left origin, sent to `vec2` mouse uniforms.
    pub cursor: [f32; 2],
    /// Shadertoy-style `iMouse`, sent to `vec4` mouse uniforms.
    pub mouse: [f32; 4],
    pub frame: i32,
}

struct Binding {
    builtin: Builtin,
    location: glow::NativeUniformLocation,
    utype: u32,
}

/// Locations and declared types of the built-in uniforms a program uses.
#[derive(Default)]
pub struct Uniforms {
    bindings: Vec<Binding>,
}

impl Uniforms {
    /// Finds the built-in uniforms a linked program uses. A uniform declared
    /// with a type the runtime can't set is reported and left unset.
    pub fn query(
        gl: &glow::Context,
        program: glow::NativeProgram,
        warnings: &mut Vec<String>,
    ) -> Self {
        let mut bindings = Vec::new();

        for index in 0..unsafe { gl.get_active_uniforms(program) } {
            let Some(active) = (unsafe { gl.get_active_uniform(program, index) }) else {
                continue;
            };
            let Some(builtin) = Builtin::ALL.into_iter().find(|b| b.name() == active.name) else {
                continue;
            };

            let accepted = builtin.accepted_types();
            if active.size != 1 || !accepted.contains(&active.utype) {
                let mut declared = type_name(active.utype);
                if active.size != 1 {
                    declared = format!("{declared}[{}]", active.size);
                }
                let expected: Vec<_> = accepted.iter().map(|&t| type_name(t)).collect();
                warnings.push(format!(
                    "{} is declared as {}, but shady can only set it as {}; it will stay zero",
                    builtin.name(),
                    declared,
                    expected.join(" or ")
                ));
                continue;
            }

            if let Some(location) = unsafe { gl.get_uniform_location(program, &active.name) } {
                bindings.push(Binding {
                    builtin,
                    location,
                    utype: active.utype,
                });
            }
        }

        Self { bindings }
    }

    /// Sets each uniform in the size it was declared with. The program must be
    /// in use.
    pub fn upload(&self, gl: &glow::Context, inputs: &FrameInputs) {
        let [width, height] = inputs.resolution;
        let [cursor_x, cursor_y] = inputs.cursor;
        let [mouse_x, mouse_y, click_x, click_y] = inputs.mouse;

        for binding in &self.bindings {
            let location = Some(&binding.location);
            unsafe {
                match (binding.builtin, binding.utype) {
                    (Builtin::Time, _) => gl.uniform_1_f32(location, inputs.time),
                    (Builtin::Resolution, glow::FLOAT_VEC2) => {
                        gl.uniform_2_f32(location, width, height)
                    }
                    (Builtin::Resolution, _) => gl.uniform_3_f32(location, width, height, 1.0),
                    (Builtin::Mouse, glow::FLOAT_VEC2) => {
                        gl.uniform_2_f32(location, cursor_x, cursor_y)
                    }
                    (Builtin::Mouse, _) => {
                        gl.uniform_4_f32(location, mouse_x, mouse_y, click_x, click_y)
                    }
                    (Builtin::Frame, glow::FLOAT) => {
                        gl.uniform_1_f32(location, inputs.frame as f32)
                    }
                    (Builtin::Frame, _) => gl.uniform_1_i32(location, inputs.frame),
                }
            }
        }
    }
}

fn type_name(utype: u32) -> String {
    let name = match utype {
        glow::FLOAT => "float",
        glow::FLOAT_VEC2 => "vec2",
        glow::FLOAT_VEC3 => "vec3",
        glow::FLOAT_VEC4 => "vec4",
        glow::INT => "int",
        glow::INT_VEC2 => "ivec2",
        glow::INT_VEC3 => "ivec3",
        glow::INT_VEC4 => "ivec4",
        glow::UNSIGNED_INT => "uint",
        glow::BOOL => "bool",
        _ => return format!("GL type 0x{utype:04X}"),
    };
    name.to_owned()
}
