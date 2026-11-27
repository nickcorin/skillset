//! The record of which skill folders `skillset` manages inside an install folder.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::InstallError;
use crate::lock::FolderName;

/// `.skillset-manifest.json`: the skill folders the last install placed, so a skill
/// dropped from the lock can be removed without touching folders it never placed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Manifest {
    pub(super) skills: BTreeSet<FolderName>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestJson {
    version: u64,
    skills: Vec<String>,
}

impl Manifest {
    pub(super) const FILE_NAME: &str = ".skillset-manifest.json";
    const VERSION: u64 = 1;

    /// Reads the manifest of `install_dir`; `None` when there is none.
    pub(super) fn read(install_dir: &Path) -> Result<Option<Self>, InstallError> {
        let path = install_dir.join(Self::FILE_NAME);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(InstallError::io("read", &path)(source)),
        };
        let invalid = |problem| InstallError::Manifest {
            path: path.clone(),
            problem,
        };
        let json: ManifestJson =
            serde_json::from_slice(&bytes).map_err(|err| invalid(ManifestProblem::Syntax(err)))?;
        if json.version != Self::VERSION {
            return Err(invalid(ManifestProblem::UnsupportedVersion(json.version)));
        }
        let skills = json
            .skills
            .into_iter()
            .map(|name| {
                FolderName::parse(&name)
                    .ok_or_else(|| invalid(ManifestProblem::InvalidFolderName(name)))
            })
            .collect::<Result<_, _>>()?;
        Ok(Some(Self { skills }))
    }

    pub(super) fn to_json(&self) -> Vec<u8> {
        let json = ManifestJson {
            version: Self::VERSION,
            skills: self
                .skills
                .iter()
                .map(|name| name.as_str().to_owned())
                .collect(),
        };
        let mut bytes = serde_json::to_vec_pretty(&json).expect("a manifest always serializes");
        bytes.push(b'\n');
        bytes
    }
}

/// Why an existing manifest was rejected.
#[derive(Debug)]
pub enum ManifestProblem {
    Syntax(serde_json::Error),
    UnsupportedVersion(u64),
    InvalidFolderName(String),
}

impl fmt::Display for ManifestProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(source) => write!(f, "not a valid skillset manifest: {source}"),
            Self::UnsupportedVersion(version) => write!(
                f,
                "unsupported manifest version {version} (expected {})",
                Manifest::VERSION
            ),
            Self::InvalidFolderName(name) => write!(f, "{name:?} is not a skill folder name"),
        }
    }
}
