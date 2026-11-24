use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crate::shader::ShaderSet;

/// How often watch mode checks the shader files for changes.
const WATCH_INTERVAL: Duration = Duration::from_millis(200);

/// The shader files on disk, and their modification times as of the last load.
pub struct ShaderFiles {
    fragment: TrackedFile,
    vertex: Option<TrackedFile>,
}

struct TrackedFile {
    path: PathBuf,
    loaded_mtime: Option<SystemTime>,
}

impl TrackedFile {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            loaded_mtime: None,
        }
    }

    fn mtime(&self) -> Result<SystemTime, String> {
        std::fs::metadata(&self.path)
            .and_then(|metadata| metadata.modified())
            .map_err(|err| format!("failed to stat {}: {}", self.path.display(), err))
    }

    fn read(&self) -> Result<String, String> {
        std::fs::read_to_string(&self.path)
            .map_err(|err| format!("failed to read {}: {}", self.path.display(), err))
    }
}

impl ShaderFiles {
    pub fn new(fragment_path: PathBuf, vertex_path: Option<PathBuf>) -> Self {
        Self {
            fragment: TrackedFile::new(fragment_path),
            vertex: vertex_path.map(TrackedFile::new),
        }
    }

    /// Reads and assembles the shaders. Each file's mtime is taken before it
    /// is read, so a save that lands during the read still counts as a change.
    pub fn load(&mut self) -> Result<ShaderSet, String> {
        let fragment_mtime = self.fragment.mtime().ok();
        let fragment = self.fragment.read()?;

        let (vertex, vertex_mtime) = match &self.vertex {
            Some(file) => {
                let mtime = file.mtime().ok();
                (Some(file.read()?), mtime)
            }
            None => (None, None),
        };

        self.fragment.loaded_mtime = fragment_mtime;
        if let Some(file) = &mut self.vertex {
            file.loaded_mtime = vertex_mtime;
        }

        Ok(ShaderSet::assemble(&fragment, vertex.as_deref()))
    }

    /// Whether any file's mtime differs from the last load. Compares for
    /// inequality, so a file restored with an older mtime still counts.
    pub fn changed(&self) -> Result<bool, String> {
        for file in self.vertex.iter().chain([&self.fragment]) {
            if Some(file.mtime()?) != file.loaded_mtime {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The file paths, for log messages.
    pub fn describe(&self) -> String {
        let fragment = self.fragment.path.display();
        match &self.vertex {
            Some(vertex) => format!("{} + {}", vertex.path.display(), fragment),
            None => fragment.to_string(),
        }
    }
}

/// Reloads the shader files when they change, checking at most every
/// `WATCH_INTERVAL`.
pub struct Watcher {
    files: ShaderFiles,
    last_check: Instant,
    /// The last problem returned, so a lasting one is reported once.
    last_error: Option<String>,
}

impl Watcher {
    pub fn new(files: ShaderFiles) -> Self {
        Self {
            files,
            last_check: Instant::now(),
            last_error: None,
        }
    }

    /// Returns the reloaded shaders after a file changed, or an error the
    /// first time a problem appears. Editors that save by replacing the file
    /// can leave it missing for a moment, so the same error isn't repeated.
    pub fn poll(&mut self) -> Option<Result<ShaderSet, String>> {
        if self.last_check.elapsed() < WATCH_INTERVAL {
            return None;
        }
        self.last_check = Instant::now();

        let loaded = match self.files.changed() {
            Ok(true) => self.files.load().map(Some),
            Ok(false) => Ok(None),
            Err(err) => Err(err),
        };

        match loaded {
            Ok(shaders) => {
                self.last_error = None;
                shaders.map(Ok)
            }
            Err(err) if self.last_error.as_ref() == Some(&err) => None,
            Err(err) => {
                self.last_error = Some(err.clone());
                Some(Err(err))
            }
        }
    }
}
