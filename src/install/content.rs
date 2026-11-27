//! What one path inside the install folder should hold, compared with and written to disk.

use std::collections::BTreeMap;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::snapshot::SkillFile;

/// The contents of a managed path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Content {
    /// A folder holding exactly these files, keyed by `/`-separated relative path.
    Folder(BTreeMap<String, SkillFile>),
    File(Vec<u8>),
}

impl Content {
    /// Whether `path` already holds exactly this content. A folder matches only when
    /// it has the same files with the same bytes (and, on Unix, the same executable
    /// bit) and nothing else: no other files, symlinks or empty folders.
    pub(super) fn is_at(&self, path: &Path) -> io::Result<bool> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err),
        };
        match self {
            Self::File(bytes) => Ok(metadata.is_file() && fs::read(path)? == *bytes),
            Self::Folder(files) => {
                if !metadata.is_dir() {
                    return Ok(false);
                }
                let installed = InstalledFolder::read(path)?;
                let same_files = installed.files.len() == files.len()
                    && installed.files.iter().zip(files).all(
                        |((installed_path, installed_file), (path, file))| {
                            installed_path == path
                                && installed_file.contents == file.contents
                                && (!cfg!(unix) || installed_file.executable == file.executable)
                        },
                    );
                Ok(installed.holds_only_files && same_files)
            }
        }
    }

    /// Writes this content to `path`, which must not exist yet.
    pub(super) fn write_to(&self, path: &Path) -> io::Result<()> {
        match self {
            Self::File(bytes) => fs::write(path, bytes),
            Self::Folder(files) => {
                fs::create_dir(path)?;
                for (relative, file) in files {
                    let target = relative
                        .split('/')
                        .fold(path.to_path_buf(), |parent, part| parent.join(part));
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(&target, &file.contents)?;
                    #[cfg(unix)]
                    if file.executable {
                        let mut permissions = fs::metadata(&target)?.permissions();
                        let mode = permissions.mode();
                        permissions.set_mode(mode | ((mode & 0o444) >> 2));
                        fs::set_permissions(&target, permissions)?;
                    }
                }
                Ok(())
            }
        }
    }
}

/// The regular files found under a folder on disk.
struct InstalledFolder {
    files: BTreeMap<String, SkillFile>,
    /// False when the folder also holds symlinks, special files, empty folders or
    /// names that are not UTF-8, none of which an installed skill contains.
    holds_only_files: bool,
}

impl InstalledFolder {
    fn read(path: &Path) -> io::Result<Self> {
        let mut folder = Self {
            files: BTreeMap::new(),
            holds_only_files: true,
        };
        folder.read_entries(path, "")?;
        Ok(folder)
    }

    fn read_entries(&mut self, folder: &Path, prefix: &str) -> io::Result<()> {
        let mut is_empty = true;
        for entry in fs::read_dir(folder)? {
            let entry = entry?;
            is_empty = false;
            let Ok(name) = entry.file_name().into_string() else {
                self.holds_only_files = false;
                continue;
            };
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                self.read_entries(&entry.path(), &format!("{prefix}{name}/"))?;
            } else if file_type.is_file() {
                #[cfg(unix)]
                let executable = entry.metadata()?.permissions().mode() & 0o111 != 0;
                #[cfg(not(unix))]
                let executable = false;
                let contents = fs::read(entry.path())?;
                self.files.insert(
                    format!("{prefix}{name}"),
                    SkillFile {
                        contents,
                        executable,
                    },
                );
            } else {
                self.holds_only_files = false;
            }
        }
        if is_empty && !prefix.is_empty() {
            self.holds_only_files = false;
        }
        Ok(())
    }
}
