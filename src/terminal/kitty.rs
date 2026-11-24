//! The parts of the kitty graphics protocol that shady uses:
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Image id for the frames. Each frame replaces the image with this id.
pub const IMAGE_ID: u32 = 1;

/// Raw bytes per chunk when sending pixels through the tty. 3072 bytes encode
/// to 4096 bytes of base64, the largest chunk the protocol allows.
const RAW_CHUNK_SIZE: usize = 3072;

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
    TempFile { dir: PathBuf, counter: u64 },
}

impl Transport {
    pub fn describe(&self) -> &'static str {
        match self {
            Transport::Direct => "direct",
            Transport::TempFile { .. } => "temp files",
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

/// Asks whether the terminal accepts a 1x1 RGB image sent directly.
pub fn query_direct(id: u32) -> String {
    // "AAAA" is one black RGB pixel.
    format!("\x1b_Gi={id},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\")
}

/// Asks whether the terminal can read a 1x1 RGB image from the temp file at
/// `path`.
pub fn query_temp_file(id: u32, path: &Path) -> String {
    let path = base64(path.as_os_str().as_bytes());
    format!("\x1b_Gi={id},s=1,v=1,a=q,t=t,f=24;{path}\x1b\\")
}

/// Deletes the frame image and frees its data.
pub fn write_delete(out: &mut impl Write) -> io::Result<()> {
    write!(out, "\x1b_Ga=d,d=I,i={IMAGE_ID},q=2\x1b\\")
}

/// Writes one RGB frame at the cursor, replacing the previous frame and
/// scaled to fill `cols` x `rows` cells. The cursor doesn't move. The terminal
/// replies once it has loaded the frame.
pub fn write_frame(
    out: &mut impl Write,
    transport: &mut Transport,
    rgb: &[u8],
    size: [u32; 2],
    cells: [u16; 2],
) -> io::Result<()> {
    let [width, height] = size;
    let [cols, rows] = cells;
    let header = format!("a=T,f=24,s={width},v={height},i={IMAGE_ID},p=1,c={cols},r={rows},C=1");

    if let Transport::TempFile { dir, counter } = transport {
        // A new name per frame, so a file isn't rewritten while the terminal
        // is still reading it.
        let path = dir.join(temp_file_name(&counter.to_string()));
        *counter += 1;
        match std::fs::write(&path, rgb) {
            Ok(()) => {
                let path = base64(path.as_os_str().as_bytes());
                return write!(out, "\x1b_G{header},t=t;{path}\x1b\\");
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
        if first {
            write!(out, "\x1b_G{header},m={more};")?;
            first = false;
        } else {
            write!(out, "\x1b_Gm={more};")?;
        }
        encoded.clear();
        base64_into(chunk, &mut encoded);
        out.write_all(&encoded)?;
        out.write_all(b"\x1b\\")?;
    }
    Ok(())
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

    #[test]
    fn splits_direct_frames_into_chunks() {
        let rgb = vec![7u8; RAW_CHUNK_SIZE * 2 + 3];
        let mut out = Vec::new();
        write_frame(&mut out, &mut Transport::Direct, &rgb, [1, 2], [3, 4]).unwrap();
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
