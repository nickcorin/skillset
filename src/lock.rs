//! `skills-lock.json` as written by `npx skills` (lock version 2), plus an optional
//! `skillset` object that `npx skills` does not read.
//!
//! ```json
//! {
//!   "version": 2,
//!   "skillset": { "installDir": "vendor/skills" },
//!   "skills": {
//!     "pdf-tools": {
//!       "source": "owner/repo",
//!       "sourceType": "github",
//!       "commit": "b45dbefda7e1098d90f140b803059d6ecbc367dc",
//!       "skillPath": "skills/pdf-tools/SKILL.md",
//!       "computedHash": "2be34cd34b36a1831c3df36e8c60fae86b73027c30a79245408706d4fadb926f"
//!     }
//!   }
//! }
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::content_hash::ContentHash;

/// The only lock version whose `computedHash` this crate computes.
pub const LOCK_VERSION: u64 = 2;

const GITHUB_SOURCE_TYPE: &str = "github";

/// A parsed `skills-lock.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lockfile {
    /// `skillset.installDir`: the default install folder, relative to the lock's folder.
    pub install_dir: Option<PathBuf>,
    /// Entries sorted by skill name.
    pub entries: Vec<LockEntry>,
}

/// One skill restored from a GitHub repository at a pinned commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockEntry {
    /// The key in the lock's `skills` object.
    pub name: String,
    pub folder_name: FolderName,
    pub source: GitHubRepo,
    pub commit: CommitSha,
    pub skill_path: SkillPath,
    pub computed_hash: ContentHash,
}

/// A GitHub repository recorded as `owner/repo` in an entry's `source`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitHubRepo {
    owner: String,
    repo: String,
}

impl GitHubRepo {
    /// Accepts `owner/repo` as `npx skills` validates it, with an optional `.git` suffix.
    pub fn parse(source: &str) -> Option<Self> {
        let (owner, repo) = source.split_once('/')?;
        let repo = repo.strip_suffix(".git").unwrap_or(repo);
        let owner_is_valid = !owner.is_empty()
            && owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        let repo_is_valid = !repo.is_empty()
            && repo != "."
            && repo != ".."
            && repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        (owner_is_valid && repo_is_valid).then(|| Self {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
        })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }
}

impl fmt::Display for GitHubRepo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

/// A full 40-hex-digit Git commit SHA.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitSha(String);

impl CommitSha {
    pub fn parse(sha: &str) -> Option<Self> {
        let is_full_sha = sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit());
        is_full_sha.then(|| Self(sha.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommitSha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The repository-relative path of a skill's `SKILL.md`, such as `skills/pdf/SKILL.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillPath(String);

impl SkillPath {
    /// Accepts a relative `/`-separated path without `.` or `..` parts whose last part
    /// is `SKILL.md` (in any letter case).
    pub fn parse(path: &str) -> Option<Self> {
        let parts_are_plain = path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\\'));
        let names_skill_file = path
            .rsplit('/')
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"));
        (parts_are_plain && names_skill_file).then(|| Self(path.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The skill's folder within the repository; empty for a skill at the root.
    pub fn folder(&self) -> &str {
        self.0.rsplit_once('/').map_or("", |(folder, _)| folder)
    }

    /// The name of the `SKILL.md` file as recorded.
    pub fn file_name(&self) -> &str {
        self.0.rsplit_once('/').map_or(&self.0, |(_, name)| name)
    }
}

impl fmt::Display for SkillPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The folder a skill installs into, named the way `npx skills` names it
/// (`sanitizeName` in its `src/sanitize.ts`). Always a single path component that
/// does not start with `.`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FolderName(String);

impl FolderName {
    /// Lowercases the skill name, turns each run of characters other than
    /// `a-z 0-9 . _` into `-`, trims leading and trailing `.` and `-`, keeps at most
    /// 255 characters, and falls back to `unnamed-skill` when nothing is left.
    pub fn for_skill(name: &str) -> Self {
        let mut sanitized = String::with_capacity(name.len());
        let mut in_replaced_run = false;
        for ch in name.to_lowercase().chars() {
            if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_' {
                sanitized.push(ch);
                in_replaced_run = false;
            } else if !in_replaced_run {
                sanitized.push('-');
                in_replaced_run = true;
            }
        }
        let trimmed = sanitized.trim_matches(['.', '-']);
        let truncated = &trimmed[..trimmed.len().min(255)];
        if truncated.is_empty() {
            Self("unnamed-skill".to_owned())
        } else {
            Self(truncated.to_owned())
        }
    }

    /// Accepts a name only if it is already in the form `for_skill` produces.
    pub fn parse(name: &str) -> Option<Self> {
        let folder = Self::for_skill(name);
        (folder.0 == name).then_some(folder)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FolderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Lockfile {
    pub fn read(path: &Path) -> Result<Self, LockError> {
        let json = fs::read_to_string(path).map_err(|source| LockError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&json).map_err(|source| LockError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    pub fn parse(json: &str) -> Result<Self, LockParseError> {
        let raw: RawLockfile = serde_json::from_str(json).map_err(LockParseError::Syntax)?;
        if raw.version != LOCK_VERSION {
            return Err(LockProblem::UnsupportedVersion(raw.version).into());
        }
        let install_dir = raw
            .skillset
            .map(|settings| {
                if settings.install_dir.is_empty() {
                    Err(LockProblem::EmptyInstallDir)
                } else {
                    Ok(PathBuf::from(settings.install_dir))
                }
            })
            .transpose()?;

        let mut entries = Vec::with_capacity(raw.skills.len());
        let mut names_by_folder: BTreeMap<FolderName, String> = BTreeMap::new();
        for (name, raw_entry) in raw.skills {
            let entry = raw_entry.into_entry(name)?;
            if let Some(other) =
                names_by_folder.insert(entry.folder_name.clone(), entry.name.clone())
            {
                return Err(LockProblem::SharedFolder {
                    first: other,
                    second: entry.name,
                    folder: entry.folder_name.0,
                }
                .into());
            }
            entries.push(entry);
        }
        Ok(Self {
            install_dir,
            entries,
        })
    }
}

#[derive(Deserialize)]
struct RawLockfile {
    version: u64,
    skills: BTreeMap<String, RawLockEntry>,
    skillset: Option<RawSkillsetSettings>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawSkillsetSettings {
    install_dir: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLockEntry {
    source: Option<String>,
    source_type: Option<String>,
    commit: Option<String>,
    skill_path: Option<String>,
    computed_hash: Option<String>,
    skill_name: Option<String>,
}

impl RawLockEntry {
    fn into_entry(self, name: String) -> Result<LockEntry, LockProblem> {
        let required = |field: &'static str, value: Option<String>| {
            value.ok_or_else(|| LockProblem::MissingField {
                skill: name.clone(),
                field,
            })
        };
        let invalid = |field: &'static str, value: String, expected: &'static str| {
            LockProblem::InvalidField {
                skill: name.clone(),
                field,
                value,
                expected,
            }
        };

        let source_type = required("sourceType", self.source_type)?;
        if source_type != GITHUB_SOURCE_TYPE {
            return Err(LockProblem::UnsupportedSourceType {
                skill: name.clone(),
                source_type,
            });
        }
        if let Some(source_name) = self.skill_name {
            return Err(LockProblem::RenamedSkill {
                skill: name.clone(),
                source_name,
            });
        }

        let source = required("source", self.source)?;
        let source =
            GitHubRepo::parse(&source).ok_or_else(|| invalid("source", source, "owner/repo"))?;
        let commit = required("commit", self.commit)?;
        let commit = CommitSha::parse(&commit)
            .ok_or_else(|| invalid("commit", commit, "a full 40-character hex commit SHA"))?;
        let skill_path = required("skillPath", self.skill_path)?;
        let skill_path = SkillPath::parse(&skill_path).ok_or_else(|| {
            invalid(
                "skillPath",
                skill_path,
                "a relative path to SKILL.md such as skills/<name>/SKILL.md",
            )
        })?;
        let computed_hash = required("computedHash", self.computed_hash)?;
        let computed_hash = ContentHash::parse(&computed_hash).ok_or_else(|| {
            invalid(
                "computedHash",
                computed_hash,
                "a lowercase hex SHA-256 digest",
            )
        })?;

        Ok(LockEntry {
            folder_name: FolderName::for_skill(&name),
            name,
            source,
            commit,
            skill_path,
            computed_hash,
        })
    }
}

/// Why a lock file could not be used.
#[derive(Debug)]
pub enum LockError {
    Read {
        path: PathBuf,
        source: io::Error,
    },
    Parse {
        path: PathBuf,
        source: LockParseError,
    },
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::Parse { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for LockError {}

/// Why lock file contents were rejected.
#[derive(Debug)]
pub enum LockParseError {
    Syntax(serde_json::Error),
    Invalid(LockProblem),
}

impl From<LockProblem> for LockParseError {
    fn from(problem: LockProblem) -> Self {
        Self::Invalid(problem)
    }
}

impl fmt::Display for LockParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(source) => write!(f, "not a valid skills lock file: {source}"),
            Self::Invalid(problem) => problem.fmt(f),
        }
    }
}

impl std::error::Error for LockParseError {}

/// Well-formed JSON that is not a lock this crate can install from.
#[derive(Debug, PartialEq, Eq)]
pub enum LockProblem {
    UnsupportedVersion(u64),
    EmptyInstallDir,
    MissingField {
        skill: String,
        field: &'static str,
    },
    InvalidField {
        skill: String,
        field: &'static str,
        value: String,
        expected: &'static str,
    },
    UnsupportedSourceType {
        skill: String,
        source_type: String,
    },
    RenamedSkill {
        skill: String,
        source_name: String,
    },
    SharedFolder {
        first: String,
        second: String,
        folder: String,
    },
}

impl fmt::Display for LockProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(
                    f,
                    "unsupported lock version {version} (expected {LOCK_VERSION})"
                )
            }
            Self::EmptyInstallDir => f.write_str("\"skillset.installDir\" is empty"),
            Self::MissingField { skill, field } => {
                write!(f, "skill \"{skill}\" has no \"{field}\"")
            }
            Self::InvalidField {
                skill,
                field,
                value,
                expected,
            } => write!(
                f,
                "skill \"{skill}\" has invalid \"{field}\" {value:?} (expected {expected})"
            ),
            Self::UnsupportedSourceType { skill, source_type } => write!(
                f,
                "skill \"{skill}\" has sourceType \"{source_type}\"; only \"{GITHUB_SOURCE_TYPE}\" sources can be installed"
            ),
            Self::RenamedSkill { skill, source_name } => write!(
                f,
                "skill \"{skill}\" is installed under a new name (skillName \"{source_name}\"); \
                 renamed skills cannot be installed because their SKILL.md differs from the source"
            ),
            Self::SharedFolder {
                first,
                second,
                folder,
            } => write!(
                f,
                "skills \"{first}\" and \"{second}\" would both install into folder \"{folder}\""
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        CommitSha, FolderName, GitHubRepo, LockParseError, LockProblem, Lockfile, SkillPath,
    };

    /// A lock with one GitHub entry named `pdf`, edited field by field.
    struct LockFixture {
        lock: Value,
    }

    impl LockFixture {
        fn new() -> Self {
            Self {
                lock: json!({
                    "version": 2,
                    "skills": {
                        "pdf": {
                            "source": "owner/repo",
                            "sourceType": "github",
                            "commit": "b45dbefda7e1098d90f140b803059d6ecbc367dc",
                            "skillPath": "skills/pdf/SKILL.md",
                            "computedHash": "2be34cd34b36a1831c3df36e8c60fae86b73027c30a79245408706d4fadb926f"
                        }
                    }
                }),
            }
        }

        fn with(mut self, pointer: &str, value: Value) -> Self {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            self.lock.pointer_mut(parent).unwrap()[key] = value;
            self
        }

        fn without(mut self, pointer: &str) -> Self {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            self.lock
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
            self
        }

        fn parse(&self) -> Result<Lockfile, LockParseError> {
            Lockfile::parse(&self.lock.to_string())
        }

        fn problem(&self) -> LockProblem {
            match self.parse() {
                Err(LockParseError::Invalid(problem)) => problem,
                other => panic!("expected an invalid lock, got {other:?}"),
            }
        }
    }

    #[test]
    fn parses_a_github_entry() {
        let lock = LockFixture::new().parse().unwrap();
        assert_eq!(lock.install_dir, None);
        let [entry] = lock.entries.as_slice() else {
            panic!("expected one entry");
        };
        assert_eq!(entry.name, "pdf");
        assert_eq!(entry.folder_name.as_str(), "pdf");
        assert_eq!(entry.source.to_string(), "owner/repo");
        assert_eq!(
            entry.commit.as_str(),
            "b45dbefda7e1098d90f140b803059d6ecbc367dc"
        );
        assert_eq!(entry.skill_path.folder(), "skills/pdf");
        assert_eq!(
            entry.computed_hash.as_str(),
            "2be34cd34b36a1831c3df36e8c60fae86b73027c30a79245408706d4fadb926f"
        );
    }

    #[test]
    fn reads_install_dir_from_skillset_object() {
        let lock = LockFixture::new()
            .with("/skillset", json!({"installDir": "vendor/skills"}))
            .parse()
            .unwrap();
        assert_eq!(lock.install_dir.as_deref(), Some("vendor/skills".as_ref()));
    }

    #[test]
    fn rejects_unknown_or_empty_skillset_settings() {
        let unknown = LockFixture::new().with("/skillset", json!({"installdir": "x"}));
        assert!(matches!(unknown.parse(), Err(LockParseError::Syntax(_))));
        let empty = LockFixture::new().with("/skillset", json!({"installDir": ""}));
        assert_eq!(empty.problem(), LockProblem::EmptyInstallDir);
    }

    #[test]
    fn ignores_fields_it_does_not_use() {
        let lock = LockFixture::new()
            .with("/futureField", json!(true))
            .with("/skills/pdf/ref", json!("main"))
            .with("/skills/pdf/installMode", json!("copy"))
            .parse()
            .unwrap();
        assert_eq!(lock.entries.len(), 1);
    }

    #[test]
    fn rejects_other_lock_versions() {
        for version in [1, 3] {
            assert_eq!(
                LockFixture::new()
                    .with("/version", json!(version))
                    .problem(),
                LockProblem::UnsupportedVersion(version)
            );
        }
    }

    #[test]
    fn rejects_malformed_json() {
        for json in [
            "",
            "{",
            "[]",
            r#"{"version": 2}"#,
            r#"{"skills": {}}"#,
            r#"{"version": "2", "skills": {}}"#,
        ] {
            assert!(
                matches!(Lockfile::parse(json), Err(LockParseError::Syntax(_))),
                "accepted {json:?}"
            );
        }
    }

    #[test]
    fn rejects_entries_missing_required_fields() {
        for field in [
            "source",
            "sourceType",
            "commit",
            "skillPath",
            "computedHash",
        ] {
            assert_eq!(
                LockFixture::new()
                    .without(&format!("/skills/pdf/{field}"))
                    .problem(),
                LockProblem::MissingField {
                    skill: "pdf".to_owned(),
                    field
                },
                "field {field}"
            );
        }
    }

    #[test]
    fn rejects_entries_with_invalid_fields() {
        let cases = [
            ("source", "https://github.com/owner/repo"),
            ("source", "owner"),
            ("commit", "b45dbef"),
            ("commit", "main"),
            ("skillPath", "skills/pdf"),
            ("skillPath", "/skills/pdf/SKILL.md"),
            ("skillPath", "skills/../SKILL.md"),
            ("computedHash", ""),
            ("computedHash", "XYZ"),
        ];
        for (field, value) in cases {
            let problem = LockFixture::new()
                .with(&format!("/skills/pdf/{field}"), json!(value))
                .problem();
            assert!(
                matches!(problem, LockProblem::InvalidField { field: f, .. } if f == field),
                "{field} = {value:?}"
            );
        }
    }

    #[test]
    fn rejects_non_github_sources() {
        assert_eq!(
            LockFixture::new()
                .with("/skills/pdf/sourceType", json!("local"))
                .problem(),
            LockProblem::UnsupportedSourceType {
                skill: "pdf".to_owned(),
                source_type: "local".to_owned()
            }
        );
    }

    #[test]
    fn rejects_renamed_skills() {
        let problem = LockFixture::new()
            .with("/skills/pdf/skillName", json!("pdf-original"))
            .problem();
        assert!(matches!(problem, LockProblem::RenamedSkill { .. }));
    }

    #[test]
    fn rejects_names_that_share_a_folder() {
        let entry = LockFixture::new().lock["skills"]["pdf"].clone();
        let problem = LockFixture::new()
            .without("/skills/pdf")
            .with("/skills/My PDF", entry.clone())
            .with("/skills/my-pdf", entry)
            .problem();
        assert_eq!(
            problem,
            LockProblem::SharedFolder {
                first: "My PDF".to_owned(),
                second: "my-pdf".to_owned(),
                folder: "my-pdf".to_owned()
            }
        );
    }

    #[test]
    fn github_repo_follows_npx_owner_repo_rules() {
        assert_eq!(GitHubRepo::parse("owner/repo.git").unwrap().repo(), "repo");
        assert!(GitHubRepo::parse("my-org/repo_name.v2").is_some());
        for invalid in [
            "owner/..",
            "owner/.",
            "own_er/repo",
            "owner/re/po",
            "/repo",
            "owner/",
        ] {
            assert!(GitHubRepo::parse(invalid).is_none(), "accepted {invalid}");
        }
    }

    #[test]
    fn commit_sha_must_be_full_length() {
        assert!(CommitSha::parse("B45DBEFDA7E1098D90F140B803059D6ECBC367DC").is_some());
        assert!(CommitSha::parse("b45dbefda7e1098d90f140b803059d6ecbc367d").is_none());
        assert!(CommitSha::parse("g45dbefda7e1098d90f140b803059d6ecbc367dc").is_none());
    }

    #[test]
    fn skill_path_splits_folder_and_file_name() {
        let nested = SkillPath::parse("skills/pdf/SKILL.md").unwrap();
        assert_eq!(
            (nested.folder(), nested.file_name()),
            ("skills/pdf", "SKILL.md")
        );
        let root = SkillPath::parse("SKILL.md").unwrap();
        assert_eq!((root.folder(), root.file_name()), ("", "SKILL.md"));
        assert!(SkillPath::parse("skills//pdf/SKILL.md").is_none());
        assert!(SkillPath::parse("skills\\pdf\\SKILL.md").is_none());
    }

    #[test]
    fn folder_names_follow_npx_sanitize_name() {
        let cases = [
            ("pdf-tools", "pdf-tools"),
            ("My Skill", "my-skill"),
            ("../../etc", "etc"),
            ("a  b__c..d", "a-b__c..d"),
            (".hidden-", "hidden"),
            ("!!!", "unnamed-skill"),
        ];
        for (name, folder) in cases {
            assert_eq!(FolderName::for_skill(name).as_str(), folder, "{name}");
        }
        assert_eq!(FolderName::for_skill(&"x".repeat(300)).as_str().len(), 255);
    }

    #[test]
    fn folder_name_parse_accepts_only_sanitized_names() {
        assert!(FolderName::parse("pdf-tools").is_some());
        for unsafe_name in ["..", "../x", ".licenses", "a/b", "My Skill", ""] {
            assert!(FolderName::parse(unsafe_name).is_none(), "{unsafe_name:?}");
        }
    }
}
