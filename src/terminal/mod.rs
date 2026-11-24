//! `-k`: renders offscreen and shows the frames in the terminal with the kitty
//! graphics protocol.

mod headless;
mod kitty;
mod tty;

use std::io::{self, BufWriter, Write};
use std::time::{Duration, Instant};

use crate::files::Watcher;
use crate::renderer::{LoadReport, Renderer};
use crate::shader::ShaderSet;
use headless::Offscreen;
use kitty::{InputParser, Reply, Transport};
use tty::{Size, Tty};

/// Fastest frame rate. Waiting for the terminal to acknowledge each frame
/// sets the actual pace.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
/// How long to wait for an acknowledgement before sending the next frame
/// anyway.
const ACK_TIMEOUT: Duration = Duration::from_secs(1);
/// Longest wait for input, so file changes and resizes are still noticed.
const MAX_WAIT: Duration = Duration::from_millis(50);
/// How long to wait for answers to the startup queries.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// Cell size in pixels to assume when the terminal doesn't report its size.
const FALLBACK_CELL_SIZE: [f64; 2] = [8.0, 16.0];
/// How long a notice such as "Reloaded" stays in the status line.
const NOTICE_DURATION: Duration = Duration::from_secs(3);

/// Image ids for the startup queries, distinct from `kitty::IMAGE_ID`.
const DIRECT_QUERY_ID: u32 = 31;
const TEMP_FILE_QUERY_ID: u32 = 32;

pub fn run(name: String, shaders: ShaderSet, watcher: Option<Watcher>) -> Result<(), String> {
    // Set up GL before taking over the terminal, so failures print normally.
    let (_context, gl) = headless::create_context()?;
    let mut renderer = Renderer::new(gl);
    let report = renderer.use_shaders(shaders);

    let tty = Tty::open()?;
    let mut out = BufWriter::with_capacity(1 << 16, io::stdout());
    let result = probe(&tty, &mut out).and_then(|transport| {
        let mut session = Session::new(renderer, watcher, name, transport);
        session.apply_report(report, false);
        let result = session.run(&tty, &mut out);
        session.finish(&tty, &mut out);
        result.map(|()| session.messages)
    });
    let _ = out.flush();
    drop(out);
    // Leave the alternate screen before printing anything.
    drop(tty);

    let messages = result?;
    for message in messages {
        eprintln!("{message}");
    }
    Ok(())
}

/// Checks that the terminal supports the kitty graphics protocol, and whether
/// it can read frames from temp files on this machine.
fn probe(tty: &Tty, out: &mut impl Write) -> Result<Transport, String> {
    let replies = query(tty, out, &kitty::query_direct(DIRECT_QUERY_ID))?;
    if !replies
        .iter()
        .any(|reply| reply.id == Some(DIRECT_QUERY_ID) && reply.is_ok())
    {
        let mut message = "this terminal doesn't support the kitty graphics protocol".to_owned();
        if std::env::var_os("TMUX").is_some() {
            message.push_str(" (tmux doesn't pass it through)");
        }
        return Err(message);
    }

    let dir = kitty::temp_dir();
    let path = dir.join(kitty::temp_file_name("probe"));
    if std::fs::write(&path, [0u8; 3]).is_err() {
        return Ok(Transport::Direct);
    }
    let replies = query(tty, out, &kitty::query_temp_file(TEMP_FILE_QUERY_ID, &path));
    // The terminal deletes the file if it read it; over SSH it can't.
    let _ = std::fs::remove_file(&path);

    let readable = replies?
        .iter()
        .any(|reply| reply.id == Some(TEMP_FILE_QUERY_ID) && reply.is_ok());
    Ok(if readable {
        Transport::TempFile { dir, counter: 0 }
    } else {
        Transport::Direct
    })
}

/// Sends a graphics query and returns the replies to it.
fn query(tty: &Tty, out: &mut impl Write, command: &str) -> Result<Vec<Reply>, String> {
    out.write_all(command.as_bytes()).map_err(write_error)?;
    sync(tty, out, QUERY_TIMEOUT)
}

/// Requests the device attributes and returns the graphics replies that arrive
/// before them. Terminals answer in order, so once the device attributes
/// arrive, everything sent earlier has been answered.
fn sync(tty: &Tty, out: &mut impl Write, timeout: Duration) -> Result<Vec<Reply>, String> {
    out.write_all(kitty::DEVICE_ATTRIBUTES_REQUEST.as_bytes())
        .and_then(|()| out.flush())
        .map_err(write_error)?;

    let deadline = Instant::now() + timeout;
    let mut parser = InputParser::default();
    let mut replies = Vec::new();
    let mut other = Vec::new();
    while !kitty::contains_device_attributes(&other) {
        let wait = deadline.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            return Err("the terminal didn't answer a device attributes query".to_owned());
        }
        let input = tty.read(wait).map_err(read_error)?;
        let (new_replies, new_other) = parser.feed(&input);
        replies.extend(new_replies);
        other.extend(new_other);
    }
    Ok(replies)
}

/// Where the image and status line go for a terminal size.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Layout {
    cols: u16,
    /// Rows the image covers. The status line is the row below.
    image_rows: u16,
    status_row: u16,
    /// Image size in pixels.
    size: [u32; 2],
}

impl Layout {
    fn new(terminal: Size) -> Self {
        let cols = terminal.cols.max(1);
        let rows = terminal.rows.max(2);
        let image_rows = rows - 1;

        let [cell_width, cell_height] = if terminal.width > 0 && terminal.height > 0 {
            [
                f64::from(terminal.width) / f64::from(cols),
                f64::from(terminal.height) / f64::from(rows),
            ]
        } else {
            FALLBACK_CELL_SIZE
        };
        let pixels =
            |cells: u16, cell_size: f64| ((f64::from(cells) * cell_size).round() as u32).max(1);

        Self {
            cols,
            image_rows,
            status_row: rows,
            size: [pixels(cols, cell_width), pixels(image_rows, cell_height)],
        }
    }
}

/// Counts frames sent per second.
struct FpsCounter {
    window_start: Instant,
    frames: u32,
    /// `None` until a full second has been measured.
    fps: Option<u32>,
}

impl FpsCounter {
    fn new() -> Self {
        Self {
            window_start: Instant::now(),
            frames: 0,
            fps: None,
        }
    }

    fn tick(&mut self) {
        self.frames += 1;
        let elapsed = self.window_start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            self.fps = Some((f64::from(self.frames) / elapsed.as_secs_f64()).round() as u32);
            self.frames = 0;
            self.window_start = Instant::now();
        }
    }
}

struct Session {
    renderer: Renderer,
    watcher: Option<Watcher>,
    name: String,
    transport: Transport,
    parser: InputParser,

    layout: Option<Layout>,
    offscreen: Option<Offscreen>,
    rgb: Vec<u8>,
    next_frame: Instant,
    /// When the last frame was sent, until the terminal acknowledges it.
    in_flight: Option<Instant>,
    fps: FpsCounter,

    /// Summary of the current shader or watch error.
    error: Option<String>,
    /// A temporary status message and its color.
    notice: Option<(String, u8, Instant)>,
    /// The status line as last drawn, to skip redundant redraws.
    status_drawn: String,
    /// Full messages from the latest shader load, printed on exit.
    messages: Vec<String>,
}

impl Session {
    fn new(
        renderer: Renderer,
        watcher: Option<Watcher>,
        name: String,
        transport: Transport,
    ) -> Self {
        Self {
            renderer,
            watcher,
            name,
            transport,
            parser: InputParser::default(),
            layout: None,
            offscreen: None,
            rgb: Vec::new(),
            next_frame: Instant::now(),
            in_flight: None,
            fps: FpsCounter::new(),
            error: None,
            notice: None,
            status_drawn: String::new(),
            messages: Vec::new(),
        }
    }

    fn run(&mut self, tty: &Tty, out: &mut impl Write) -> Result<(), String> {
        loop {
            let input = tty.read(self.wait_time()).map_err(read_error)?;
            let (replies, keys) = self.parser.feed(&input);
            for reply in replies {
                self.handle_reply(reply);
            }
            if tty::is_quit(&keys) || tty::terminate_requested() {
                return Ok(());
            }

            self.poll_watcher();
            let size = tty
                .size()
                .map_err(|err| format!("failed to get the terminal size: {err}"))?;
            self.update_layout(size, out)?;
            if self.frame_due() {
                self.send_frame(out)?;
            }
            self.draw_status(out).map_err(write_error)?;
            out.flush().map_err(write_error)?;
        }
    }

    /// Removes the image and reads any replies still on their way. The
    /// terminal itself is restored by `Tty`.
    fn finish(&mut self, tty: &Tty, out: &mut impl Write) {
        if let Some(offscreen) = self.offscreen.take() {
            offscreen.delete(self.renderer.gl());
        }
        // The last frame may not be acknowledged yet. A reply that arrives
        // after the terminal is restored shows up at the shell prompt as if
        // typed, so wait for it here.
        let _ = kitty::write_delete(out);
        let _ = sync(tty, out, ACK_TIMEOUT);
    }

    fn wait_time(&self) -> Duration {
        if self.in_flight.is_some() {
            // The acknowledgement wakes us up.
            return MAX_WAIT;
        }
        self.next_frame
            .saturating_duration_since(Instant::now())
            .min(MAX_WAIT)
    }

    fn frame_due(&self) -> bool {
        Instant::now() >= self.next_frame
            && self
                .in_flight
                .is_none_or(|sent| sent.elapsed() >= ACK_TIMEOUT)
    }

    fn handle_reply(&mut self, reply: Reply) {
        if reply.id != Some(kitty::IMAGE_ID) {
            return;
        }
        self.in_flight = None;
        if reply.is_ok() {
            return;
        }

        let message = format!("Terminal rejected a frame: {}", reply.message);
        if matches!(self.transport, Transport::TempFile { .. }) {
            // Temp files worked during the probe; fall back rather than stop.
            self.transport = Transport::Direct;
            self.set_notice(format!("{message}; sending frames directly"), 33);
        } else {
            self.set_notice(message.clone(), 31);
        }
        self.messages.push(message);
    }

    fn poll_watcher(&mut self) {
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        match watcher.poll() {
            None => {}
            Some(Ok(shaders)) => {
                let report = self.renderer.use_shaders(shaders);
                self.apply_report(report, true);
            }
            Some(Err(err)) => {
                self.error = Some(summarize(&err));
                self.messages.push(format!("Watch warning: {err}"));
            }
        }
    }

    fn apply_report(&mut self, report: LoadReport, reloaded: bool) {
        self.messages = report.messages();
        self.error = report.error.as_deref().map(summarize);
        if let Some(warning) = report.warnings.first() {
            self.set_notice(format!("Warning: {warning}"), 33);
        } else if reloaded && report.error.is_none() {
            self.set_notice(format!("Reloaded {}", self.name), 32);
        }
    }

    fn set_notice(&mut self, text: String, color: u8) {
        self.notice = Some((text, color, Instant::now()));
    }

    fn update_layout(&mut self, size: Size, out: &mut impl Write) -> Result<(), String> {
        let layout = Layout::new(size);
        if self.layout == Some(layout) {
            return Ok(());
        }

        let gl = self.renderer.gl();
        if let Some(offscreen) = self.offscreen.take() {
            offscreen.delete(gl);
        }
        self.offscreen = Some(Offscreen::new(gl, layout.size)?);
        self.layout = Some(layout);

        // Clear the old image and status line, and draw right away.
        kitty::write_delete(out)
            .and_then(|()| out.write_all(b"\x1b[2J"))
            .map_err(write_error)?;
        self.status_drawn.clear();
        self.next_frame = Instant::now();
        Ok(())
    }

    fn send_frame(&mut self, out: &mut impl Write) -> Result<(), String> {
        let (Some(layout), Some(offscreen)) = (self.layout, &mut self.offscreen) else {
            return Ok(());
        };

        offscreen.bind(self.renderer.gl());
        self.renderer.draw(layout.size, [0.0; 2], [0.0; 4]);
        offscreen.read_rgb(self.renderer.gl(), &mut self.rgb);

        out.write_all(b"\x1b[H")
            .and_then(|()| {
                kitty::write_frame(
                    out,
                    &mut self.transport,
                    &self.rgb,
                    layout.size,
                    [layout.cols, layout.image_rows],
                )
            })
            .map_err(write_error)?;

        let now = Instant::now();
        self.in_flight = Some(now);
        self.next_frame = now + FRAME_INTERVAL;
        self.fps.tick();
        Ok(())
    }

    fn draw_status(&mut self, out: &mut impl Write) -> io::Result<()> {
        let Some(layout) = self.layout else {
            return Ok(());
        };

        let (text, color) = if let Some(error) = &self.error {
            (format!("✗ {error}"), Some(31))
        } else if let Some((notice, color, since)) = &self.notice
            && since.elapsed() < NOTICE_DURATION
        {
            (notice.clone(), Some(*color))
        } else {
            // The name goes last, so a long path is what gets cut off.
            let fps = self
                .fps
                .fps
                .map(|fps| format!("{fps} fps · "))
                .unwrap_or_default();
            (
                format!(
                    "q to quit · {fps}{} · {}",
                    self.transport.describe(),
                    self.name
                ),
                None,
            )
        };

        let text: String = text.chars().take(usize::from(layout.cols)).collect();
        let line = match color {
            Some(color) => format!("\x1b[{color}m{text}\x1b[0m"),
            None => text,
        };
        if line == self.status_drawn {
            return Ok(());
        }
        write!(out, "\x1b[{};1H\x1b[2K{line}", layout.status_row)?;
        self.status_drawn = line;
        Ok(())
    }
}

/// The first two non-empty lines of a message, on one line. For compile
/// errors that's the heading and the first line of the driver's log.
fn summarize(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

fn read_error(err: io::Error) -> String {
    format!("failed to read from the terminal: {err}")
}

fn write_error(err: io::Error) -> String {
    format!("failed to write to the terminal: {err}")
}
