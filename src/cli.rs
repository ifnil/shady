use std::ffi::OsString;
use std::path::PathBuf;

pub const USAGE: &str = "\
Usage: shady [options] <fragment.glsl>

Options:
  -w, --watch            Reload the shaders when their files change
  -v, --vertex <file>    Use a custom vertex shader
  -h, --help             Show this help";

#[derive(Debug)]
pub struct CliOptions {
    pub fragment_path: PathBuf,
    pub vertex_path: Option<PathBuf>,
    pub watch_enabled: bool,
}

#[derive(Debug)]
pub enum CliError {
    Help,
    Invalid(String),
}

pub fn parse_args() -> Result<CliOptions, CliError> {
    parse(std::env::args_os().skip(1))
}

fn invalid(message: impl Into<String>) -> CliError {
    CliError::Invalid(message.into())
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<CliOptions, CliError> {
    let mut watch_enabled = false;
    let mut vertex_path: Option<PathBuf> = None;
    let mut fragment_path: Option<PathBuf> = None;
    let mut only_paths = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        let flag = arg
            .to_str()
            .filter(|arg| !only_paths && arg.starts_with('-'));
        match flag {
            None => {
                if fragment_path.replace(PathBuf::from(&arg)).is_some() {
                    return Err(invalid("only one fragment shader can be given"));
                }
            }
            Some("--") => only_paths = true,
            Some("-h" | "--help") => return Err(CliError::Help),
            Some("-w" | "--watch") => watch_enabled = true,
            Some(flag @ ("-v" | "--vertex")) => {
                let Some(path) = args.next() else {
                    return Err(invalid(format!("missing file after {flag}")));
                };
                if vertex_path.replace(PathBuf::from(path)).is_some() {
                    return Err(invalid("only one vertex shader can be given"));
                }
            }
            Some(flag) => return Err(invalid(format!("unknown option: {flag}"))),
        }
    }

    let fragment_path = fragment_path.ok_or_else(|| invalid("no fragment shader given"))?;

    Ok(CliOptions {
        fragment_path,
        vertex_path,
        watch_enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<CliOptions, CliError> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_flags_and_paths() {
        let options = parse_strs(&["-w", "--vertex", "a.vert", "b.frag"]).unwrap();
        assert!(options.watch_enabled);
        assert_eq!(options.vertex_path, Some(PathBuf::from("a.vert")));
        assert_eq!(options.fragment_path, PathBuf::from("b.frag"));
    }

    #[test]
    fn double_dash_ends_options() {
        let options = parse_strs(&["--", "-odd.glsl"]).unwrap();
        assert_eq!(options.fragment_path, PathBuf::from("-odd.glsl"));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(parse_strs(&["-h"]), Err(CliError::Help)));
        assert!(matches!(parse_strs(&[]), Err(CliError::Invalid(_))));
        assert!(matches!(parse_strs(&["a", "b"]), Err(CliError::Invalid(_))));
        assert!(matches!(parse_strs(&["-v"]), Err(CliError::Invalid(_))));
        assert!(matches!(
            parse_strs(&["-x", "a"]),
            Err(CliError::Invalid(_))
        ));
    }
}
