use glow::HasContext;
use glutin::api::egl::context::PossiblyCurrentContext;
use glutin::api::egl::device::Device;
use glutin::api::egl::display::Display;
use glutin::config::{Api, ConfigSurfaceTypes, ConfigTemplateBuilder};
use glutin::context::{ContextApi, ContextAttributesBuilder, Version};
use glutin::display::GlDisplay;

/// Keeps a windowless OpenGL context current while it lives.
pub struct HeadlessContext {
    _context: PossiblyCurrentContext,
    _display: Display,
}

/// Creates an OpenGL 3.3 context with no window, so it needs no display
/// server. Uses the first EGL device that can create one; Mesa lists GPUs
/// before its software renderer.
pub fn create_context() -> Result<(HeadlessContext, glow::Context), String> {
    let devices =
        Device::query_devices().map_err(|err| format!("failed to list EGL devices: {err}"))?;

    let mut errors = Vec::new();
    for device in devices {
        match create_on_device(&device) {
            Ok(created) => return Ok(created),
            Err(err) => {
                let name = device.name().or(device.vendor()).unwrap_or("EGL device");
                errors.push(format!("{name}: {err}"));
            }
        }
    }
    if errors.is_empty() {
        errors.push("no EGL devices found".to_owned());
    }
    Err(format!(
        "failed to create a headless OpenGL 3.3 context:\n  {}",
        errors.join("\n  ")
    ))
}

fn create_on_device(device: &Device) -> Result<(HeadlessContext, glow::Context), String> {
    let display = unsafe { Display::with_device(device, None) }.map_err(|err| err.to_string())?;

    let template = ConfigTemplateBuilder::new()
        .with_api(Api::OPENGL)
        .with_surface_type(ConfigSurfaceTypes::empty())
        .build();
    let config = unsafe { display.find_configs(template) }
        .map_err(|err| err.to_string())?
        .next()
        .ok_or("no OpenGL config")?;

    let context_attrs = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 3))))
        .build(None);
    let context = unsafe { display.create_context(&config, &context_attrs) }
        .and_then(|context| context.make_current_surfaceless())
        .map_err(|err| err.to_string())?;

    let gl =
        unsafe { glow::Context::from_loader_function_cstr(|name| display.get_proc_address(name)) };
    Ok((
        HeadlessContext {
            _context: context,
            _display: display,
        },
        gl,
    ))
}

/// A color and depth framebuffer to render into and read back from.
pub struct Offscreen {
    framebuffer: glow::NativeFramebuffer,
    color: glow::NativeRenderbuffer,
    depth: glow::NativeRenderbuffer,
    size: [u32; 2],
    /// Pixels as read from GL, bottom row first.
    readback: Vec<u8>,
}

impl Offscreen {
    pub fn new(gl: &glow::Context, size: [u32; 2]) -> Result<Self, String> {
        let [width, height] = size.map(|n| n as i32);
        unsafe {
            let framebuffer = gl.create_framebuffer()?;
            let color = gl.create_renderbuffer()?;
            let depth = gl.create_renderbuffer()?;

            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(color));
            gl.renderbuffer_storage(glow::RENDERBUFFER, glow::RGBA8, width, height);
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
            gl.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT24, width, height);
            gl.bind_renderbuffer(glow::RENDERBUFFER, None);

            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            let attach = |attachment, renderbuffer| {
                gl.framebuffer_renderbuffer(
                    glow::FRAMEBUFFER,
                    attachment,
                    glow::RENDERBUFFER,
                    Some(renderbuffer),
                )
            };
            attach(glow::COLOR_ATTACHMENT0, color);
            attach(glow::DEPTH_ATTACHMENT, depth);
            let complete =
                gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE;

            let offscreen = Self {
                framebuffer,
                color,
                depth,
                size,
                readback: Vec::new(),
            };
            if !complete {
                offscreen.delete(gl);
                return Err(format!("failed to create a {width}x{height} framebuffer"));
            }
            Ok(offscreen)
        }
    }

    pub fn delete(self, gl: &glow::Context) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.delete_framebuffer(self.framebuffer);
            gl.delete_renderbuffer(self.color);
            gl.delete_renderbuffer(self.depth);
        }
    }

    /// Directs drawing (and reading) to this framebuffer.
    pub fn bind(&self, gl: &glow::Context) {
        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.framebuffer)) };
    }

    /// Reads the image into `rgb` as tightly packed rows, top row first.
    pub fn read_rgb(&mut self, gl: &glow::Context, rgb: &mut Vec<u8>) {
        let [width, height] = self.size;
        let stride = width as usize * 3;
        self.readback.resize(stride * height as usize, 0);
        unsafe {
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.read_pixels(
                0,
                0,
                width as i32,
                height as i32,
                glow::RGB,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut self.readback)),
            );
        }

        // GL's rows start at the bottom; the terminal wants the top first.
        rgb.clear();
        for row in self.readback.chunks_exact(stride).rev() {
            rgb.extend_from_slice(row);
        }
    }
}
