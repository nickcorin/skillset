//! A skill named in the lock, and the files that matched its `computedHash`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use flate2::read::GzDecoder;
use serde::Deserialize;

use crate::content_hash::ContentHash;
use crate::error::Error;

const HOPS: u32 = 40;

#[derive(Clone)]
struct File {
    bytes: Vec<u8>,
    executable: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum SourceKind {
    Github,
}

/// Where the lock says the skill is, and the files once they have been fetched.
#[derive(Deserialize)]
pub struct Skill {
    #[serde(skip)]
    pub(crate) name: String,
    pub(crate) source: String,
    pub(crate) commit: String,
    #[serde(rename = "skillPath")]
    pub(crate) skill_path: String,
    #[serde(rename = "computedHash")]
    pub(crate) hash: ContentHash,
    #[serde(rename = "sourceType")]
    _source_type: SourceKind,
    #[serde(skip)]
    files: BTreeMap<String, File>,
}

impl Skill {
    /// Keeps this skill's files from a codeload tarball when they hash to `computedHash`.
    ///
    /// A relative symlink becomes the file it names, up to 40 hops, when that file stays
    /// inside the skill folder. Anything else is left out, as are `metadata.json`, `.git`,
    /// `__pycache__`, and `__pypackages__`.
    pub(crate) fn take(&mut self, archive: &[u8]) -> Result<(), Error> {
        fn broken(source: io::Error) -> Error {
            Error::new(format!("cannot read archive: {source}"))
        }
        fn plain(path: &str) -> bool {
            !path.is_empty()
                && path.split('/').all(|part| {
                    !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':'])
                })
        }
        fn kept<'a>(root: &str, path: &'a str) -> Option<&'a str> {
            let rel = if root.is_empty() {
                path
            } else {
                path.strip_prefix(root)?.strip_prefix('/')?
            };
            let excluded = rel.split('/').any(|part| {
                matches!(
                    part,
                    "" | "." | ".." | "metadata.json" | ".git" | "__pycache__" | "__pypackages__"
                )
            });
            (!excluded).then_some(rel)
        }
        fn lookup<'a>(
            files: &'a BTreeMap<String, File>,
            links: &'a BTreeMap<String, String>,
            root: &str,
            start: &str,
        ) -> Option<&'a File> {
            let mut path = start.to_owned();
            for _ in 0..HOPS {
                let under = if root.is_empty() {
                    !path.is_empty()
                } else {
                    path.strip_prefix(root)
                        .is_some_and(|rest| rest.starts_with('/'))
                };
                if !under {
                    return None;
                }
                if let Some(file) = files.get(&path) {
                    return Some(file);
                }
                let target = links.get(&path)?;
                if Path::new(target).is_absolute() {
                    return None;
                }
                let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                let mut parts: Vec<&str> = if parent.is_empty() {
                    Vec::new()
                } else {
                    parent.split('/').collect()
                };
                for part in target.split('/') {
                    match part {
                        "" | "." => {}
                        ".." => {
                            parts.pop()?;
                        }
                        other => parts.push(other),
                    }
                }
                path = parts.join("/");
            }
            None
        }

        let mut tar = tar::Archive::new(GzDecoder::new(archive));
        let mut archived = BTreeMap::new();
        let mut links = BTreeMap::new();
        for entry in tar.entries().map_err(broken)? {
            let mut entry = entry.map_err(broken)?;
            let kind = entry.header().entry_type();
            if kind.is_pax_global_extensions() {
                continue;
            }
            let owned = {
                let raw = entry.path_bytes();
                let path = std::str::from_utf8(&raw).map_err(|_| {
                    Error::new(format!(
                        "archive path {:?} is not UTF-8",
                        String::from_utf8_lossy(&raw)
                    ))
                })?;
                path.trim_end_matches('/').to_owned()
            };
            let Some((_, inner)) = owned.split_once('/') else {
                continue;
            };
            if inner.is_empty() {
                continue;
            }
            if !plain(inner) {
                return Err(Error::new(format!(
                    "archive path {inner:?} must be relative, with no empty, \".\", or \"..\" \
                     component, and no '\\' or ':'"
                )));
            }
            if kind.is_dir() {
                continue;
            }
            if kind.is_symlink() {
                let raw = entry.link_name_bytes().ok_or_else(|| {
                    Error::new(format!(
                        "archive entry {inner:?} is not a file, folder, or symlink"
                    ))
                })?;
                let target = std::str::from_utf8(&raw).map_err(|_| {
                    Error::new(format!(
                        "archive path {:?} is not UTF-8",
                        String::from_utf8_lossy(&raw)
                    ))
                })?;
                links.insert(inner.to_owned(), target.to_owned());
            } else if kind.is_file() || kind.is_contiguous() {
                let mode = entry.header().mode().map_err(broken)?;
                let mut body = Vec::new();
                entry.read_to_end(&mut body).map_err(broken)?;
                archived.insert(
                    inner.to_owned(),
                    File {
                        bytes: body,
                        executable: mode & 0o111 != 0,
                    },
                );
            } else {
                return Err(Error::new(format!(
                    "archive entry {inner:?} is not a file, folder, or symlink"
                )));
            }
        }

        let root = self.skill_path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let mut files = BTreeMap::new();
        for (path, file) in &archived {
            if let Some(rel) = kept(root, path) {
                files.insert(rel.to_owned(), file.clone());
            }
        }
        for path in links.keys() {
            let Some(rel) = kept(root, path) else {
                continue;
            };
            let Some(file) = lookup(&archived, &links, root, path) else {
                continue;
            };
            files.insert(rel.to_owned(), file.clone());
        }

        let skill_file = self
            .skill_path
            .rsplit_once('/')
            .map_or(self.skill_path.as_str(), |(_, name)| name);
        if !files.contains_key(skill_file) {
            return Err(Error::new(format!("not found at {}", self.skill_path)));
        }
        let actual = ContentHash::of_files(
            files
                .iter()
                .map(|(path, file)| (path.as_str(), file.bytes.as_slice())),
        );
        if actual != self.hash {
            return Err(Error::new(format!(
                "does not match skills-lock.json: computedHash is {} but the fetched files hash to {actual}",
                self.hash
            )));
        }
        self.files = files;
        Ok(())
    }

    pub(crate) fn matches(&self, dir: &Path) -> Result<bool, Error> {
        fn executable(path: &Path) -> io::Result<bool> {
            #[cfg(unix)]
            {
                Ok(fs::metadata(path)?.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            {
                let _ = path;
                Ok(false)
            }
        }
        fn read(dir: &Path) -> Result<Option<BTreeMap<String, File>>, Error> {
            let mut files = BTreeMap::new();
            let mut pending = vec![(dir.to_path_buf(), String::new())];
            while let Some((folder, prefix)) = pending.pop() {
                for entry in
                    fs::read_dir(&folder).map_err(|source| Error::io("read", &folder, source))?
                {
                    let entry = entry.map_err(|source| Error::io("read", &folder, source))?;
                    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                        return Ok(None);
                    };
                    let rel = if prefix.is_empty() {
                        name
                    } else {
                        format!("{prefix}/{name}")
                    };
                    let kind = entry
                        .file_type()
                        .map_err(|source| Error::io("read", &entry.path(), source))?;
                    if kind.is_symlink() || !(kind.is_dir() || kind.is_file()) {
                        return Ok(None);
                    }
                    if kind.is_dir() {
                        pending.push((entry.path(), rel));
                        continue;
                    }
                    let bytes = fs::read(entry.path())
                        .map_err(|source| Error::io("read", &entry.path(), source))?;
                    let marked = executable(&entry.path())
                        .map_err(|source| Error::io("read", &entry.path(), source))?;
                    files.insert(
                        rel,
                        File {
                            bytes,
                            executable: marked,
                        },
                    );
                }
            }
            Ok(Some(files))
        }

        match fs::symlink_metadata(dir) {
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(Error::io("read", dir, source)),
            Ok(meta) if !meta.is_dir() => return Ok(false),
            Ok(_) => {}
        }
        let Some(found) = read(dir)? else {
            return Ok(false);
        };
        if found.len() != self.files.len() {
            return Ok(false);
        }
        for (path, file) in &self.files {
            let Some(got) = found.get(path) else {
                return Ok(false);
            };
            if got.bytes != file.bytes || (cfg!(unix) && got.executable != file.executable) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn place(&self, dir: &Path) -> Result<(), Error> {
        fs::create_dir(dir).map_err(|source| Error::io("create", dir, source))?;
        for (rel, file) in &self.files {
            let mut target = dir.to_path_buf();
            for part in rel.split('/') {
                target.push(part);
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|source| Error::io("create", parent, source))?;
            }
            fs::write(&target, &file.bytes)
                .map_err(|source| Error::io("write", &target, source))?;
            #[cfg(unix)]
            if file.executable {
                let mut permissions = fs::metadata(&target)
                    .map_err(|source| Error::io("write", &target, source))?
                    .permissions();
                let mode = permissions.mode();
                permissions.set_mode(mode | ((mode & 0o444) >> 2));
                fs::set_permissions(&target, permissions)
                    .map_err(|source| Error::io("write", &target, source))?;
            }
        }
        Ok(())
    }
}
