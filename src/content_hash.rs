use std::fmt;

use sha2::{Digest, Sha256};

use crate::skill::SkillFile;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentHash(String);

impl ContentHash {
    pub(crate) fn from_lock(value: String) -> Self {
        Self(value)
    }

    pub fn of(files: &[SkillFile]) -> Self {
        let mut entries = Vec::with_capacity(files.len());
        for file in files {
            let path = file.path.as_bytes().to_vec();
            let content = Self::normalized_content(&file.content);
            entries.push((path, content));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        let mut hasher = Sha256::new();
        for (path, content) in &entries {
            hasher.update(path);
            hasher.update([0]);
            hasher.update(content.len().to_string().into_bytes());
            hasher.update([0]);
            hasher.update(content);
        }
        Self(Self::hex(&hasher.finalize()))
    }

    fn normalized_content(content: &[u8]) -> Vec<u8> {
        if content.contains(&0) {
            return content.to_vec();
        }
        let mut normalized = Vec::with_capacity(content.len());
        let mut index = 0;
        while index < content.len() {
            if content[index] == b'\r' && content.get(index + 1) == Some(&b'\n') {
                normalized.push(b'\n');
                index += 2;
            } else {
                normalized.push(content[index]);
                index += 1;
            }
        }
        normalized
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::ContentHash;
    use crate::skill::SkillFile;

    fn file(path: &str, content: &[u8], executable: bool) -> SkillFile {
        SkillFile {
            path: path.to_string(),
            content: content.to_vec(),
            executable,
        }
    }

    #[test]
    fn empty_file_list_matches_the_documented_hash() {
        assert_eq!(
            ContentHash::of(&[]).to_string(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn single_skill_file_matches_the_documented_hash() {
        let files = [file("SKILL.md", b"# hi\n", false)];
        assert_eq!(
            ContentHash::of(&files).to_string(),
            "93c36952d8843b33e68ae970ebabcc03e1687c9bb0dd1ecaf357060318952f55"
        );
    }

    #[test]
    fn two_files_match_the_documented_hash_without_reordering_input() {
        let files = [
            file("docs/b.md", b"b\n", false),
            file("SKILL.md", b"a\n", false),
        ];
        assert_eq!(
            ContentHash::of(&files).to_string(),
            "b22c4d22aa319d2924312fa18fcbfedc88fa89c74dbd8f298c683e1caf7b7f24"
        );
        assert_eq!(files[0].path, "docs/b.md");
        assert_eq!(files[1].path, "SKILL.md");
        assert_eq!(files[0].content, b"b\n");
    }

    #[test]
    fn crlf_text_hashes_like_lf_and_keeps_original_bytes() {
        let crlf = [file("SKILL.md", b"# hi\r\nthere\r\n", false)];
        let lf = [file("SKILL.md", b"# hi\nthere\n", false)];
        assert_eq!(ContentHash::of(&crlf), ContentHash::of(&lf));
        assert_eq!(
            ContentHash::of(&lf).to_string(),
            "89413e281ace6e573d4f65a9829c7e178c49f2489f7b3a87e73d6e9977a0f5a7"
        );
        assert_eq!(crlf[0].content, b"# hi\r\nthere\r\n");
    }

    #[test]
    fn nul_file_keeps_crlf_in_the_hash() {
        let crlf = [file("a.bin", &[0, 13, 10], false)];
        let lf = [file("a.bin", &[0, 10], false)];
        assert_eq!(
            ContentHash::of(&crlf).to_string(),
            "ca7ddc68e58a79aecb1f5aede04b7af0496cfcd07502050668c16c0634a363b2"
        );
        assert_eq!(
            ContentHash::of(&lf).to_string(),
            "9f4ad3e25abb9c76cc738bdb5f855a7d658862b4c967e6f39a573505cff20899"
        );
        assert_eq!(crlf[0].content, [0, 13, 10]);
    }

    #[test]
    fn paths_sort_by_utf8_bytes() {
        let files = [
            file("b.md", b"1", false),
            file("B.md", b"2", false),
            file("ä.md", "3".as_bytes(), false),
        ];
        let reversed = [
            file("ä.md", "3".as_bytes(), false),
            file("B.md", b"2", false),
            file("b.md", b"1", false),
        ];
        assert_eq!(
            ContentHash::of(&files).to_string(),
            "88bbb8ad139e8a5da9d0e5a81a6bc584535cc8a88edeca48f6a2dce7c5c97f13"
        );
        assert_eq!(ContentHash::of(&files), ContentHash::of(&reversed));
    }

    #[test]
    fn executable_bit_does_not_change_the_hash() {
        let executable = [file("SKILL.md", b"# hi\n", true)];
        let plain = [file("SKILL.md", b"# hi\n", false)];
        assert_eq!(ContentHash::of(&executable), ContentHash::of(&plain));
        assert!(executable[0].executable);
        assert!(!plain[0].executable);
    }
}
