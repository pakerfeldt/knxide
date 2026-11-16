//! Reading the project from disk: a `.knxproj` archive or an already unpacked directory.

use std::path::{Path, PathBuf};

use knxide_program::project::{MAX_FILE_SIZE, is_project_file};
use knxide_program::{Error, Project, ProjectArchive, ProjectSource};

/// An unpacked project: the directories `P-…` and `M-…` with their XML files.
struct Directory {
    root: PathBuf,
    paths: Vec<String>,
}

impl Directory {
    fn open(root: &Path) -> Result<Self, String> {
        let unreadable =
            |path: &Path, error: std::io::Error| format!("{}: {error}", path.display());
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(root).map_err(|error| unreadable(root, error))? {
            let entry = entry.map_err(|error| unreadable(root, error))?;
            let directory = entry.file_name().to_string_lossy().into_owned();
            if !(directory.starts_with("P-") || directory.starts_with("M-"))
                || !entry.path().is_dir()
            {
                continue;
            }
            let inner = entry.path();
            for file in std::fs::read_dir(&inner).map_err(|error| unreadable(&inner, error))? {
                let file = file.map_err(|error| unreadable(&inner, error))?;
                let path = format!("{directory}/{}", file.file_name().to_string_lossy());
                if is_project_file(&path) && file.path().is_file() {
                    paths.push(path);
                }
            }
        }
        paths.sort();
        Ok(Self {
            root: root.to_path_buf(),
            paths,
        })
    }
}

impl ProjectSource for Directory {
    fn paths(&self) -> Vec<String> {
        self.paths.clone()
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, Error> {
        // Only what was listed is read: nothing outside the project, no baggage.
        if !self.paths.iter().any(|known| known == path) {
            return Err(Error::Missing(format!("project file {path}")));
        }
        let file = self.root.join(path);
        let failed = |error: std::io::Error| Error::Archive(format!("{}: {error}", file.display()));
        let size = std::fs::metadata(&file).map_err(failed)?.len();
        if size > MAX_FILE_SIZE as u64 {
            return Err(Error::Archive(format!(
                "{} is larger than a project file may be",
                file.display()
            )));
        }
        std::fs::read(&file).map_err(failed)
    }
}

/// Load the project at `path`: a `.knxproj` file, or a directory that holds its unpacked
/// content. Only the project's XML files are read; a password-protected project is refused.
pub fn load_project(path: &Path) -> Result<Project, String> {
    load_project_in(path, None)
}

/// Like [`load_project`], with the products' texts in `language` (for example `en-US`) where
/// they have them.
pub fn load_project_in(path: &Path, language: Option<&str>) -> Result<Project, String> {
    let named = |error: knxide_program::Error| format!("{}: {error}", path.display());
    if path.is_dir() {
        let directory = Directory::open(path)?;
        return Project::load_in(&directory, language).map_err(named);
    }
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let archive = ProjectArchive::open(&bytes).map_err(named)?;
    Project::load_in(&archive, language).map_err(named)
}
