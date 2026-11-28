use std::path::PathBuf;

use crate::error::Error;
use crate::lockfile::Lockfile;

#[derive(Debug)]
pub struct Installer {
    pub dir: PathBuf,
}

impl Installer {
    pub fn install(&self, lockfile: &Lockfile) -> Result<(), Error> {
        let mut fetched = Vec::with_capacity(lockfile.skills.len());
        for skill in &lockfile.skills {
            fetched.push(skill.fetch()?);
        }
        for (skill, files) in lockfile.skills.iter().zip(fetched) {
            skill.write(&self.dir, &files)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::Installer;
    use crate::lockfile::Lockfile;

    fn temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("skillset-install-{nanos}"));
        fs::create_dir_all(&path).expect("temp");
        path
    }

    fn stamp(path: &std::path::Path, secs: u64) -> SystemTime {
        let time = UNIX_EPOCH + Duration::from_secs(secs);
        fs::File::open(path)
            .expect("open")
            .set_modified(time)
            .expect("mtime");
        time
    }

    #[test]
    fn empty_lock_leaves_existing_directory_and_sibling_and_skips_missing_dir() {
        let parent = temp_dir();
        let dir = parent.join("skills");
        fs::create_dir_all(&dir).expect("dir");
        let sibling = parent.join("sibling.txt");
        fs::write(&sibling, b"sibling").expect("sibling");
        let inner = dir.join("keep.txt");
        fs::write(&inner, b"inside").expect("inner");
        let sibling_mtime = stamp(&sibling, 1_700_000_000);
        let inner_mtime = stamp(&inner, 1_700_000_100);
        let dir_mtime = stamp(&dir, 1_700_000_200);
        let lock = Lockfile { skills: Vec::new() };

        Installer { dir: dir.clone() }
            .install(&lock)
            .expect("install");

        assert_eq!(fs::read(&sibling).expect("read"), b"sibling");
        assert_eq!(fs::read(&inner).expect("read"), b"inside");
        assert_eq!(
            fs::metadata(&sibling)
                .expect("meta")
                .modified()
                .expect("mtime"),
            sibling_mtime
        );
        assert_eq!(
            fs::metadata(&inner)
                .expect("meta")
                .modified()
                .expect("mtime"),
            inner_mtime
        );
        assert_eq!(
            fs::metadata(&dir).expect("meta").modified().expect("mtime"),
            dir_mtime
        );

        let missing = parent.join("missing");
        Installer {
            dir: missing.clone(),
        }
        .install(&lock)
        .expect("missing install");
        assert!(!missing.exists());
        assert_eq!(fs::read(&sibling).expect("read"), b"sibling");
        let _ = fs::remove_dir_all(&parent);
    }
}
