//! The parts of the kitty graphics protocol that shady uses:
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/>
//!
//! Inside tmux, each command is wrapped in tmux's passthrough sequence, which
//! tmux forwards to the terminal it runs in when `allow-passthrough` is on.
//! tmux doesn't know about the image, so it can't keep it in place. Instead,
//! shady prints Unicode placeholder characters that the terminal draws the
//! image over, and tmux positions those like any other text.

use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::diacritics::ROW_DIACRITICS;

/// Raw bytes per chunk when sending pixels through the tty. 3072 bytes encode
/// to 4096 bytes of base64, the largest chunk the protocol allows.
const RAW_CHUNK_SIZE: usize = 3072;

/// The character the terminal replaces with a cell of an image.
const PLACEHOLDER: char = '\u{10EEEE}';

/// The most image rows that placeholders can number.
pub const MAX_PLACEHOLDER_ROWS: u16 = ROW_DIACRITICS.len() as u16;

/// Requests the terminal's primary device attributes. Every terminal answers
/// this, so a graphics query sent before it has been answered (or ignored)
/// once its reply arrives.
pub const DEVICE_ATTRIBUTES_REQUEST: &str = "\x1b[c";

/// How frames reach the terminal.
pub enum Transport {
    /// Base64 pixel data through the tty. Works anywhere, including over SSH.
    Direct,
    /// Pixel data in temporary files that the terminal reads and deletes.
    /// Much faster, but only works when the terminal runs on this machine.
    TempFile {
        dir: PathBuf,
        counter: u64,
        /// The latest frame's file, until it's replaced or removed.
        last: Option<PathBuf>,
    },
}

impl Transport {
    pub fn temp_files(dir: PathBuf) -> Self {
        Transport::TempFile {
            dir,
            counter: 0,
            last: None,
        }
    }

    pub fn describe(&self) -> &'static str {
        match self {
            Transport::Direct => "direct",
            Transport::TempFile { .. } => "temp files",
        }
    }

    /// Whether the terminal has read the latest frame, which it shows by
    /// deleting the frame's file. Always false when sending directly.
    pub fn frame_read(&self) -> bool {
        match self {
            Transport::TempFile { last, .. } => last.as_ref().is_none_or(|path| !path.exists()),
            Transport::Direct => false,
        }
    }

    /// Removes the latest frame's file if the terminal hasn't.
    pub fn remove_unread(&mut self) {
        if let Transport::TempFile { last, .. } = self
            && let Some(path) = last.take()
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Where to put frame files. The terminal only reads files from known
/// temporary directories, and memory-backed /dev/shm avoids disk writes.
pub fn temp_dir() -> PathBuf {
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        shm.to_owned()
    } else {
        std::env::temp_dir()
    }
}

/// A file name the terminal will agree to delete after reading.
pub fn temp_file_name(tag: &str) -> String {
    format!(
        "tty-graphics-protocol-shady-{}-{tag}.rgb",
        std::process::id()
    )
}

/// Writes graphics commands for this terminal.
#[derive(Clone, Copy)]
pub struct Graphics {
    /// Whether shady runs inside tmux. See the module docs.
    pub tmux: bool,
    /// Id of the frame image. Ids are shared by everything in the terminal
    /// window, including other tmux panes, so it varies by process.
    /// Placeholders give it as a 256-color index, so it also stays out of the
    /// 16 basic colors, which tmux may rewrite as different sequences.
    pub image_id: u32,
}

impl Graphics {
    pub fn new(tmux: bool) -> Self {
        Self {
            tmux,
            image_id: 16 + std::process::id() % 240,
        }
    }

    /// Asks whether the terminal accepts a 1x1 RGB image sent directly.
    pub fn query_direct(&self, out: &mut impl Write, id: u32) -> io::Result<()> {
        // "AAAA" is one black RGB pixel.
        self.command(out, &format!("i={id},s=1,v=1,a=q,t=d,f=24"), b"AAAA")
    }

    /// Asks whether the terminal can read a 1x1 RGB image from the temp file
    /// at `path`.
    pub fn query_temp_file(&self, out: &mut impl Write, id: u32, path: &Path) -> io::Result<()> {
        let path = base64(path.as_os_str().as_bytes());
        self.command(
            out,
            &format!("i={id},s=1,v=1,a=q,t=t,f=24"),
            path.as_bytes(),
        )
    }

    /// Deletes the frame image and frees its data.
    pub fn delete(&self, out: &mut impl Write) -> io::Result<()> {
        self.command(out, &format!("a=d,d=I,i={},q=2", self.image_id), b"")
    }

    /// Sends one RGB frame, replacing the previous one, scaled to fill
    /// `cells` (columns and rows).
    ///
    /// Outside tmux, the frame goes at the cursor, which doesn't move, and the
    /// terminal replies once it has loaded the frame. Inside tmux, the frame
    /// shows wherever the placeholders are, and the terminal doesn't reply:
    /// tmux would pass the reply on as typed input to whichever pane is
    /// active, which may not be shady's.
    pub fn write_frame(
        &self,
        out: &mut impl Write,
        transport: &mut Transport,
        rgb: &[u8],
        size: [u32; 2],
        cells: [u16; 2],
    ) -> io::Result<()> {
        let [width, height] = size;
        let [cols, rows] = cells;
        let (placement, quiet) = if self.tmux {
            ("U=1", "q=2,")
        } else {
            ("C=1", "")
        };
        let header = format!(
            "a=T,f=24,s={width},v={height},i={},p=1,c={cols},r={rows},{placement},{quiet}",
            self.image_id
        );

        if let Transport::TempFile { dir, counter, last } = transport {
            // The terminal hasn't read the previous frame in time, or
            // rejected it.
            if let Some(path) = last.take() {
                let _ = std::fs::remove_file(path);
            }
            // A new name per frame, so a file isn't rewritten while the
            // terminal is still reading it.
            let path = dir.join(temp_file_name(&counter.to_string()));
            *counter += 1;
            match std::fs::write(&path, rgb) {
                Ok(()) => {
                    let encoded = base64(path.as_os_str().as_bytes());
                    *last = Some(path);
                    return self.command(out, &format!("{header}t=t"), encoded.as_bytes());
                }
                // The temp directory may be full; sending directly still works.
                Err(_) => *transport = Transport::Direct,
            }
        }

        let mut encoded = Vec::with_capacity(RAW_CHUNK_SIZE / 3 * 4);
        let mut chunks = rgb.chunks(RAW_CHUNK_SIZE).peekable();
        let mut first = true;
        while let Some(chunk) = chunks.next() {
            let more = u8::from(chunks.peek().is_some());
            // Later chunks only repeat `q`, so the reply stays suppressed.
            let control = if first {
                format!("{header}m={more}")
            } else {
                format!("{quiet}m={more}")
            };
            first = false;
            encoded.clear();
            base64_into(chunk, &mut encoded);
            self.command(out, &control, &encoded)?;
        }
        Ok(())
    }

    /// Prints the placeholders that show the frame inside tmux, starting at
    /// the top left: `cells` (columns and rows) of placeholder characters in
    /// the image id's color. Only the first cell of each row carries the
    /// row's diacritic; the terminal numbers the rest of the row from it.
    /// Draws at most `MAX_PLACEHOLDER_ROWS` rows.
    pub fn write_placeholders(&self, out: &mut impl Write, cells: [u16; 2]) -> io::Result<()> {
        let [cols, rows] = cells;
        let rest: String =
            std::iter::repeat_n(PLACEHOLDER, usize::from(cols.saturating_sub(1))).collect();
        write!(out, "\x1b[38;5;{}m", self.image_id)?;
        for (row, diacritic) in ROW_DIACRITICS.iter().take(usize::from(rows)).enumerate() {
            write!(out, "\x1b[{};1H{PLACEHOLDER}{diacritic}{rest}", row + 1)?;
        }
        out.write_all(b"\x1b[39m")
    }

    /// Writes one graphics command. Inside tmux it's wrapped in a
    /// passthrough sequence, in which its own escape characters are doubled.
    fn command(&self, out: &mut impl Write, control: &str, payload: &[u8]) -> io::Result<()> {
        let (start, end): (&[u8], &[u8]) = if self.tmux {
            (b"\x1bPtmux;\x1b\x1b_G", b"\x1b\x1b\\\x1b\\")
        } else {
            (b"\x1b_G", b"\x1b\\")
        };
        out.write_all(start)?;
        out.write_all(control.as_bytes())?;
        if !payload.is_empty() {
            out.write_all(b";")?;
            out.write_all(payload)?;
        }
        out.write_all(end)
    }
}

pub fn base64(data: &[u8]) -> String {
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    base64_into(data, &mut out);
    String::from_utf8(out).expect("base64 is ASCII")
}

fn base64_into(data: &[u8], out: &mut Vec<u8>) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63]);
        out.push(ALPHABET[(n >> 12) as usize & 63]);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63]
        } else {
            b'='
        });
    }
}

/// A reply from the terminal to a graphics command, e.g. `OK` or
/// `ENOENT:file not found`.
#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub id: Option<u32>,
    pub message: String,
}

impl Reply {
    pub fn is_ok(&self) -> bool {
        self.message == "OK"
    }
}

/// Separates graphics replies from the rest of the terminal input.
#[derive(Default)]
pub struct InputParser {
    /// The start of a reply whose end hasn't arrived yet.
    pending: Vec<u8>,
}

impl InputParser {
    /// Returns the complete replies in `input` and everything else (such as
    /// keypresses). A reply cut off at the end is kept for the next call.
    pub fn feed(&mut self, input: &[u8]) -> (Vec<Reply>, Vec<u8>) {
        self.pending.extend_from_slice(input);
        let mut replies = Vec::new();
        let mut other = Vec::new();

        let mut rest = &self.pending[..];
        while let Some(start) = find(rest, b"\x1b_G") {
            other.extend_from_slice(&rest[..start]);
            let body = &rest[start + 3..];
            let Some(end) = find(body, b"\x1b\\") else {
                rest = &rest[start..];
                break;
            };
            replies.push(parse_reply(&body[..end]));
            rest = &body[end + 2..];
        }

        let remaining = if find(rest, b"\x1b_G") == Some(0) {
            rest.to_vec()
        } else {
            other.extend_from_slice(rest);
            Vec::new()
        };
        // A reply is short, so a huge unterminated one is garbage.
        self.pending = if remaining.len() > 4096 {
            Vec::new()
        } else {
            remaining
        };
        (replies, other)
    }
}

fn parse_reply(body: &[u8]) -> Reply {
    let body = String::from_utf8_lossy(body);
    let (keys, message) = body.split_once(';').unwrap_or((&body, ""));
    let id = keys
        .split(',')
        .find_map(|pair| pair.strip_prefix("i="))
        .and_then(|id| id.parse().ok());
    Reply {
        id,
        message: message.to_owned(),
    }
}

/// Whether `input` contains an answer to `DEVICE_ATTRIBUTES_REQUEST`, which
/// looks like `ESC [ ? 62 ; 22 c`.
pub fn contains_device_attributes(input: &[u8]) -> bool {
    let mut rest = input;
    while let Some(start) = find(rest, b"\x1b[?") {
        rest = &rest[start + 3..];
        let end = rest
            .iter()
            .position(|&b| !(b.is_ascii_digit() || b == b';'));
        if let Some(end) = end
            && rest[end] == b'c'
        {
            return true;
        }
    }
    false
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0, 0, 0]), "AAAA");
    }

    const PLAIN: Graphics = Graphics {
        tmux: false,
        image_id: 1,
    };
    const TMUX: Graphics = Graphics {
        tmux: true,
        image_id: 42,
    };

    #[test]
    fn splits_direct_frames_into_chunks() {
        let rgb = vec![7u8; RAW_CHUNK_SIZE * 2 + 3];
        let mut out = Vec::new();
        PLAIN
            .write_frame(&mut out, &mut Transport::Direct, &rgb, [1, 2], [3, 4])
            .unwrap();
        let out = String::from_utf8(out).unwrap();

        let commands: Vec<_> = out.split_terminator("\x1b\\").collect();
        assert_eq!(commands.len(), 3);
        assert!(commands[0].starts_with("\x1b_Ga=T,f=24,s=1,v=2,i=1,p=1,c=3,r=4,C=1,m=1;"));
        assert!(commands[1].starts_with("\x1b_Gm=1;"));
        assert!(commands[2].starts_with("\x1b_Gm=0;"));
        let payload_len = |c: &str| c.split_once(';').unwrap().1.len();
        assert_eq!(payload_len(commands[0]), 4096);
        assert_eq!(payload_len(commands[2]), 4);
    }

    #[test]
    fn wraps_quiet_commands_for_tmux() {
        let rgb = vec![7u8; RAW_CHUNK_SIZE + 3];
        let mut out = Vec::new();
        TMUX.write_frame(&mut out, &mut Transport::Direct, &rgb, [1, 2], [3, 4])
            .unwrap();
        let out = String::from_utf8(out).unwrap();

        // Each command is in its own passthrough sequence, with its escape
        // characters doubled.
        let (before, wrapped) = out.split_once("\x1bPtmux;").unwrap();
        assert_eq!(before, "");
        let commands: Vec<_> = wrapped
            .split("\x1bPtmux;")
            .map(|command| command.strip_suffix("\x1b\\").unwrap())
            .map(|command| command.replace("\x1b\x1b", "\x1b"))
            .collect();
        assert_eq!(commands.len(), 2);
        assert!(commands[0].starts_with("\x1b_Ga=T,f=24,s=1,v=2,i=42,p=1,c=3,r=4,U=1,q=2,m=1;"));
        assert!(commands[1].starts_with("\x1b_Gq=2,m=0;"));
        for command in &commands {
            assert!(command.ends_with("\x1b\\"));
            assert_eq!(command.matches('\x1b').count(), 2);
        }
    }

    #[test]
    fn prints_placeholders() {
        let mut out = Vec::new();
        TMUX.write_placeholders(&mut out, [3, 2]).unwrap();
        // The 2 rows by 3 columns example from kitty's docs, with each row
        // positioned.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b[38;5;42m\
             \x1b[1;1H\u{10EEEE}\u{0305}\u{10EEEE}\u{10EEEE}\
             \x1b[2;1H\u{10EEEE}\u{030D}\u{10EEEE}\u{10EEEE}\
             \x1b[39m"
        );
    }

    #[test]
    fn separates_replies_from_keys() {
        let mut parser = InputParser::default();
        let (replies, keys) = parser.feed(b"q\x1b_Gi=1,p=1;OK\x1b\\x\x1b_Gi=2;ENO");
        assert_eq!(
            replies,
            [Reply {
                id: Some(1),
                message: "OK".into()
            }]
        );
        assert_eq!(keys, b"qx");

        let (replies, keys) = parser.feed(b"ENT:no such file\x1b\\");
        assert_eq!(replies[0].id, Some(2));
        assert_eq!(replies[0].message, "ENOENT:no such file");
        assert!(keys.is_empty());

        let (replies, keys) = parser.feed(b"\x1b");
        assert!(replies.is_empty());
        assert_eq!(keys, b"\x1b");
    }

    #[test]
    fn detects_device_attributes() {
        assert!(contains_device_attributes(
            b"\x1b_Gi=1;OK\x1b\\\x1b[?62;22c"
        ));
        assert!(contains_device_attributes(b"\x1b[?1;2c"));
        assert!(!contains_device_attributes(b"\x1b[?62;22"));
        assert!(!contains_device_attributes(b"\x1b[A"));
    }
}
