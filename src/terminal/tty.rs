use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, Once, PoisonError};
use std::time::Duration;

const STDIN: libc::c_int = libc::STDIN_FILENO;
const STDOUT: libc::c_int = libc::STDOUT_FILENO;

/// Switch to the alternate screen, hide the cursor, clear.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[2J";
/// Show the cursor, leave the alternate screen.
const LEAVE: &[u8] = b"\x1b[?25h\x1b[?1049l";

/// The terminal settings to restore, while a `Tty` is open.
static SAVED: Mutex<Option<libc::termios>> = Mutex::new(None);
/// Set by SIGINT, SIGTERM and SIGHUP.
static TERMINATE: AtomicBool = AtomicBool::new(false);

/// The terminal on stdin/stdout in raw mode, showing the alternate screen.
/// The terminal is restored when this is dropped, and also on panic.
pub struct Tty(());

/// Terminal size in cells, and in pixels if the terminal reports it (0 if not).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub width: u16,
    pub height: u16,
}

impl Tty {
    pub fn open() -> Result<Self, String> {
        if unsafe { libc::isatty(STDIN) == 0 || libc::isatty(STDOUT) == 0 } {
            return Err("-k needs stdin and stdout to be a terminal".to_owned());
        }

        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(STDIN, &mut saved) } != 0 {
            return Err(os_error("read the terminal settings"));
        }
        let mut raw = saved;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(STDIN, libc::TCSAFLUSH, &raw) } != 0 {
            return Err(os_error("switch the terminal to raw mode"));
        }

        *lock_saved() = Some(saved);
        install_handlers();

        let mut out = io::stdout();
        let _ = out.write_all(ENTER).and_then(|()| out.flush());
        Ok(Tty(()))
    }

    pub fn size(&self) -> io::Result<Size> {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(STDOUT, libc::TIOCGWINSZ, &mut size) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Size {
            cols: size.ws_col,
            rows: size.ws_row,
            width: size.ws_xpixel,
            height: size.ws_ypixel,
        })
    }

    /// Waits up to `timeout` for input and returns what arrived, which may be
    /// nothing.
    pub fn read(&self, timeout: Duration) -> io::Result<Vec<u8>> {
        let mut poll_fd = libc::pollfd {
            fd: STDIN,
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        match unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) } {
            0 => return Ok(Vec::new()),
            n if n < 0 => return ignore_interrupt(io::Error::last_os_error()),
            _ => {}
        }

        let mut buf = vec![0u8; 4096];
        let n = unsafe { libc::read(STDIN, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            return ignore_interrupt(io::Error::last_os_error());
        }
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the terminal closed",
            ));
        }
        buf.truncate(n as usize);
        Ok(buf)
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        restore();
    }
}

/// Whether a signal asked shady to quit.
pub fn terminate_requested() -> bool {
    TERMINATE.load(Ordering::Relaxed)
}

/// Whether the keys include q, Ctrl-C, Ctrl-D or a lone Esc. Escape
/// sequences (arrow keys, F-keys, Alt combinations) are skipped, so that the
/// `Q` in F2's `ESC O Q` doesn't count.
pub fn is_quit(keys: &[u8]) -> bool {
    if keys == b"\x1b" {
        return true;
    }
    let mut i = 0;
    while i < keys.len() {
        match keys[i] {
            0x1b => {
                i += 1;
                match keys.get(i) {
                    // CSI: parameters, then a final byte in 0x40..=0x7e.
                    Some(b'[') => {
                        i += 1;
                        while i < keys.len() && !(0x40..=0x7e).contains(&keys[i]) {
                            i += 1;
                        }
                    }
                    // SS3: one more byte after the `O`.
                    Some(b'O') => i += 1,
                    // Alt+key: the key byte itself is last.
                    Some(_) | None => {}
                }
                // Skip the final byte as well.
            }
            b'q' | b'Q' | 0x03 | 0x04 => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn restore() {
    if let Some(saved) = lock_saved().take() {
        let mut out = io::stdout();
        let _ = out.write_all(LEAVE).and_then(|()| out.flush());
        unsafe { libc::tcsetattr(STDIN, libc::TCSAFLUSH, &saved) };
    }
}

fn lock_saved() -> MutexGuard<'static, Option<libc::termios>> {
    SAVED.lock().unwrap_or_else(PoisonError::into_inner)
}

fn ignore_interrupt(err: io::Error) -> io::Result<Vec<u8>> {
    if err.kind() == io::ErrorKind::Interrupted {
        Ok(Vec::new())
    } else {
        Err(err)
    }
}

fn os_error(action: &str) -> String {
    format!("failed to {action}: {}", io::Error::last_os_error())
}

extern "C" fn on_signal(_signal: libc::c_int) {
    TERMINATE.store(true, Ordering::Relaxed);
}

/// Restores the terminal before a panic message is printed, and turns
/// termination signals into a clean exit.
fn install_handlers() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            default_hook(info);
        }));

        let handler = on_signal as extern "C" fn(libc::c_int);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            unsafe { libc::signal(signal, handler as libc::sighandler_t) };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_quit_keys() {
        assert!(is_quit(b"q"));
        assert!(is_quit(b"\x03"));
        assert!(is_quit(b"\x1b"));
        assert!(is_quit(b"\x1b[Aq"));
        assert!(!is_quit(b"\x1b[A"));
        assert!(!is_quit(b"\x1bOQ"));
        assert!(!is_quit(b"\x1b[1;5Q"));
        assert!(!is_quit(b"\x1bq"));
        assert!(!is_quit(b"x"));
    }
}
