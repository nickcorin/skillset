//! The files of one skill. One is built only when those files hash to the lock's `computedHash`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use flate2::read::GzDecoder;

use crate::content_hash::ContentHash;
use crate::error::Error;

const HOPS: u32 = 40;

#[derive(Clone)]
struct File {
    bytes: Vec<u8>,
    executable: bool,
}

/// The files installed for one skill.
pub struct Skill {
    files: BTreeMap<String, File>,
}

impl Skill {
    /// Files of the skill whose `SKILL.md` is at `skill_path` in a codeload tarball.
    ///
    /// A symlink becomes the file it names when that file stays inside the skill folder.
    /// A link that is broken, loops, leaves the folder, or is absolute is left out.
    /// `metadata.json`, `.git`, `__pycache__`, and `__pypackages__` are left out.
    pub(crate) fn from_tarball(
        bytes: &[u8],
        skill_path: &str,
        hash: &ContentHash,
    ) -> Result<Self, Error> {
        fn plain(path: &str) -> bool {
            !path.is_empty()
                && path.split('/').all(|part| {
                    !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':'])
                })
        }
        fn relative<'a>(root: &str, path: &'a str) -> Option<&'a str> {
            if root.is_empty() {
                return Some(path);
            }
            path.strip_prefix(root)?.strip_prefix('/')
        }
        fn skip(rel: &str) -> bool {
            rel.split('/').any(|part| {
                matches!(
                    part,
                    "" | "." | ".." | "metadata.json" | ".git" | "__pycache__" | "__pypackages__"
                )
            })
        }
        fn inside(root: &str, path: &str) -> bool {
            if root.is_empty() {
                return !path.is_empty();
            }
            path.strip_prefix(root)
                .is_some_and(|rest| rest.starts_with('/'))
        }
        fn join(parent: &str, target: &str) -> Option<String> {
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
            Some(parts.join("/"))
        }
        fn lookup<'a>(
            files: &'a BTreeMap<String, File>,
            links: &'a BTreeMap<String, String>,
            root: &str,
            path: &str,
        ) -> Option<&'a File> {
            let mut path = path.to_owned();
            for _ in 0..HOPS {
                if !inside(root, &path) {
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
                path = join(parent, target)?;
            }
            None
        }

        let mut archive = tar::Archive::new(GzDecoder::new(bytes));
        let mut archived = BTreeMap::new();
        let mut links = BTreeMap::new();
        for entry in archive
            .entries()
            .map_err(|source| Error::new(format!("cannot read archive: {source}")))?
        {
            let mut entry =
                entry.map_err(|source| Error::new(format!("cannot read archive: {source}")))?;
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
                let mode = entry
                    .header()
                    .mode()
                    .map_err(|source| Error::new(format!("cannot read archive: {source}")))?;
                let mut body = Vec::new();
                entry
                    .read_to_end(&mut body)
                    .map_err(|source| Error::new(format!("cannot read archive: {source}")))?;
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

        let root = skill_path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let mut files = BTreeMap::new();
        for (path, file) in &archived {
            let Some(rel) = relative(root, path) else {
                continue;
            };
            if skip(rel) {
                continue;
            }
            files.insert(rel.to_owned(), file.clone());
        }
        for path in links.keys() {
            let Some(rel) = relative(root, path) else {
                continue;
            };
            if skip(rel) {
                continue;
            }
            let Some(file) = lookup(&archived, &links, root, path) else {
                continue;
            };
            files.insert(rel.to_owned(), file.clone());
        }

        let skill_file = skill_path
            .rsplit_once('/')
            .map_or(skill_path, |(_, name)| name);
        if !files.contains_key(skill_file) {
            return Err(Error::new(format!("not found at {skill_path}")));
        }
        let actual = ContentHash::of_files(
            files
                .iter()
                .map(|(path, file)| (path.as_str(), file.bytes.as_slice())),
        );
        if &actual != hash {
            return Err(Error::new(format!(
                "does not match skills-lock.json: computedHash is {hash} but the fetched files hash to {actual}"
            )));
        }
        Ok(Self { files })
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
