//! The `computedHash` of `skills-lock.json` version 2, as defined by `npx skills`
//! (`src/skill-hash.ts` and the "Lock File Content Hash" section of its README).

use std::borrow::Cow;
use std::fmt;

use sha2::{Digest, Sha256};

/// A lowercase hex SHA-256 digest over a skill's files.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContentHash(String);

impl ContentHash {
    /// Accepts exactly 64 lowercase hex digits, the form `npx skills` writes.
    pub fn parse(hex: &str) -> Option<Self> {
        let is_digest =
            hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        is_digest.then(|| Self(hex.to_owned()))
    }

    /// Hashes files given as (path relative to the skill folder, contents).
    ///
    /// Files are sorted by the UTF-8 bytes of their `/`-separated paths and each is
    /// fed to one SHA-256 as `path NUL length NUL contents`. Text files (no NUL byte)
    /// have CRLF replaced by LF first, and the length is counted after that.
    pub fn of_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Self {
        let mut entries: Vec<(String, Cow<'a, [u8]>)> = files
            .into_iter()
            .map(|(path, contents)| {
                let is_text = !contents.contains(&0);
                let contents = if is_text && contents.windows(2).any(|pair| pair == b"\r\n") {
                    let mut normalized = Vec::with_capacity(contents.len());
                    for (index, &byte) in contents.iter().enumerate() {
                        if byte != b'\r' || contents.get(index + 1) != Some(&b'\n') {
                            normalized.push(byte);
                        }
                    }
                    Cow::Owned(normalized)
                } else {
                    Cow::Borrowed(contents)
                };
                (path.replace('\\', "/"), contents)
            })
            .collect();
        entries.sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));

        let mut hasher = Sha256::new();
        for (path, contents) in &entries {
            hasher.update(path.as_bytes());
            hasher.update([0]);
            hasher.update(contents.len().to_string().as_bytes());
            hasher.update([0]);
            hasher.update(contents);
        }
        Self(
            hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::ContentHash;

    #[test]
    fn matches_upstream_readme_test_vectors() {
        assert_eq!(
            ContentHash::of_files(std::iter::empty()).as_str(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            ContentHash::of_files([("SKILL.md", &b"# hi\n"[..])]).as_str(),
            "93c36952d8843b33e68ae970ebabcc03e1687c9bb0dd1ecaf357060318952f55"
        );
        assert_eq!(
            ContentHash::of_files([("SKILL.md", &b"a\n"[..]), ("docs/b.md", &b"b\n"[..])]).as_str(),
            "b22c4d22aa319d2924312fa18fcbfedc88fa89c74dbd8f298c683e1caf7b7f24"
        );
    }

    #[test]
    fn input_order_does_not_matter() {
        let sorted: [(&str, &[u8]); 2] = [("SKILL.md", b"a\n"), ("docs/b.md", b"b\n")];
        let reversed: [(&str, &[u8]); 2] = [("docs/b.md", b"b\n"), ("SKILL.md", b"a\n")];
        assert_eq!(
            ContentHash::of_files(sorted),
            ContentHash::of_files(reversed)
        );
    }

    #[test]
    fn text_crlf_hashes_like_lf() {
        assert_eq!(
            ContentHash::of_files([("SKILL.md", &b"a\r\nb\r\n"[..])]),
            ContentHash::of_files([("SKILL.md", &b"a\nb\n"[..])])
        );
        assert_ne!(
            ContentHash::of_files([("SKILL.md", &b"a\r\r\n"[..])]),
            ContentHash::of_files([("SKILL.md", &b"a\n"[..])]),
            "CRLF is replaced in a single pass, like String.replaceAll"
        );
    }

    #[test]
    fn binary_crlf_is_hashed_as_is() {
        assert_ne!(
            ContentHash::of_files([("logo.bin", &b"\0a\r\n"[..])]),
            ContentHash::of_files([("logo.bin", &b"\0a\n"[..])])
        );
    }

    #[test]
    fn parse_accepts_only_lowercase_sha256_hex() {
        let digest = "93c36952d8843b33e68ae970ebabcc03e1687c9bb0dd1ecaf357060318952f55";
        assert_eq!(
            ContentHash::parse(digest).map(|h| h.to_string()),
            Some(digest.to_owned())
        );
        assert!(ContentHash::parse(&digest.to_uppercase()).is_none());
        assert!(ContentHash::parse(&digest[1..]).is_none());
        assert!(ContentHash::parse("").is_none());
    }
}
