use std::path::{Path, PathBuf};

const USAGE: &str = "Usage: shady [-w|--watch] [-v|--vertex <vertex.glsl>] <fragment.glsl>";

pub struct CliOptions {
    pub fragment_path: PathBuf,
    pub vertex_path: Option<PathBuf>,
    pub watch_enabled: bool,
}

pub fn parse_args() -> Result<CliOptions, String> {
    let mut watch_enabled = false;
    let mut vertex_path: Option<PathBuf> = None;
    let mut fragment_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-w" | "--watch" => watch_enabled = true,
            "-v" | "--vertex" => {
                let Some(path) = args.next() else {
                    return Err(format!("Missing value for {}\n{}", arg, USAGE));
                };
                if vertex_path.is_some() {
                    return Err(USAGE.to_owned());
                }
                vertex_path = Some(Path::new(&path).to_path_buf());
            }
            _ if arg.starts_with('-') => return Err(format!("Unknown flag: {}\n{}", arg, USAGE)),
            _ => {
                if fragment_path.is_some() {
                    return Err(USAGE.to_owned());
                }
                fragment_path = Some(Path::new(&arg).to_path_buf());
            }
        }
    }

    let Some(fragment_path) = fragment_path else {
        return Err(USAGE.to_owned());
    };

    Ok(CliOptions {
        fragment_path,
        vertex_path,
        watch_enabled,
    })
}
