//! A version 2 `skills-lock.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::{self, Deserializer};
use serde_json::Value;

use crate::error::Error;
use crate::skill::Skill;

/// The skills to restore, and an optional install folder.
#[derive(Deserialize)]
pub struct Lock {
    version: u64,
    #[serde(deserialize_with = "skills")]
    pub(crate) skills: Vec<Skill>,
    #[serde(default, rename = "skillset", deserialize_with = "install_dir")]
    pub(crate) install_dir: Option<PathBuf>,
}

impl Lock {
    pub fn read(path: &Path) -> Result<Self, Error> {
        let json = fs::read_to_string(path).map_err(|source| Error::io("read", path, source))?;
        let lock: Self = serde_json::from_str(&json).map_err(|source| {
            Error::new(format!(
                "{}: not a valid skills lock file: {source}",
                path.display()
            ))
        })?;
        if lock.version != 2 {
            return Err(Error::new(format!(
                "{}: unsupported lock version {} (expected 2)",
                path.display(),
                lock.version
            )));
        }
        Ok(lock)
    }
}

fn skills<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Skill>, D::Error> {
    let map = BTreeMap::<String, Skill>::deserialize(deserializer)?;
    map.into_iter()
        .map(|(name, mut skill)| {
            if name == "." || name == ".." || name.contains(['/', '\\']) {
                return Err(de::Error::custom(format!(
                    "skill {name:?} cannot be installed as a folder"
                )));
            }
            skill.name = name;
            Ok(skill)
        })
        .collect()
}

fn install_dir<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<PathBuf>, D::Error> {
    let Some(path) = BTreeMap::<String, Value>::deserialize(deserializer)?.remove("installDir")
    else {
        return Ok(None);
    };
    match path {
        Value::String(path) => Ok(Some(PathBuf::from(path))),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err(de::Error::custom("skillset.installDir must be a string"))
        }
    }
}
