//! Applying all changes to an install folder together: everything is written to a
//! staging folder and checked first, then moved into place, and moved back if a
//! later move fails.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use super::content::Content;
use super::{InstallError, InstallWarning};

/// One path inside the install folder and what it should hold; `None` removes it.
#[derive(Debug)]
pub(super) struct Change {
    pub(super) path: PathBuf,
    pub(super) content: Option<Content>,
}

/// Changes written to a staging folder inside the install folder, so every move
/// into place is a rename within one file system.
pub(super) struct Transaction {
    install_dir: PathBuf,
    staging_dir: PathBuf,
    created_install_dir: bool,
    changes: Vec<Change>,
}

/// A path that has been changed, and where its previous and new contents went.
struct Swap {
    target: PathBuf,
    previous: Option<PathBuf>,
    placed_from: Option<PathBuf>,
}

impl Transaction {
    /// Writes every new content to the staging folder and checks it reads back
    /// identically. Nothing outside the staging folder changes.
    pub(super) fn stage(install_dir: &Path, changes: Vec<Change>) -> Result<Self, InstallError> {
        let created_install_dir = !install_dir.exists();
        fs::create_dir_all(install_dir).map_err(InstallError::io("create", install_dir))?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let transaction = Self {
            install_dir: install_dir.to_owned(),
            staging_dir: install_dir.join(format!(".skillset-staging-{}-{nanos}", process::id())),
            created_install_dir,
            changes,
        };

        let staged = fs::create_dir(&transaction.staging_dir)
            .map_err(InstallError::io("create", &transaction.staging_dir))
            .and_then(|()| {
                for folder in [
                    transaction.staging_dir.join("new"),
                    transaction.staging_dir.join("old"),
                ] {
                    fs::create_dir(&folder).map_err(InstallError::io("create", &folder))?;
                }
                for (index, change) in transaction.changes.iter().enumerate() {
                    let Some(content) = &change.content else {
                        continue;
                    };
                    let staged = transaction.staged_path(index);
                    content
                        .write_to(&staged)
                        .map_err(InstallError::io("write", &staged))?;
                    if !content
                        .is_at(&staged)
                        .map_err(InstallError::io("read back", &staged))?
                    {
                        return Err(InstallError::StagedContentMismatch { path: staged });
                    }
                }
                Ok(())
            });
        match staged {
            Ok(()) => Ok(transaction),
            Err(err) => {
                transaction.discard();
                Err(err)
            }
        }
    }

    /// Moves every staged change into place. If a move fails, the changes already
    /// made are reverted before the error is returned.
    pub(super) fn commit(self) -> Result<Option<InstallWarning>, InstallError> {
        let mut swaps = Vec::with_capacity(self.changes.len());
        for (index, change) in self.changes.iter().enumerate() {
            let target = self.install_dir.join(&change.path);
            let mut swap = Swap {
                target,
                previous: None,
                placed_from: None,
            };
            let swapped = self.swap(index, change, &mut swap);
            swaps.push(swap);
            if let Err(err) = swapped {
                return Err(self.roll_back(swaps, err));
            }
        }
        Ok(fs::remove_dir_all(&self.staging_dir)
            .err()
            .map(|source| InstallWarning::NotRemoved {
                path: self.staging_dir.clone(),
                source,
            }))
    }

    fn swap(&self, index: usize, change: &Change, swap: &mut Swap) -> Result<(), InstallError> {
        match fs::symlink_metadata(&swap.target) {
            Ok(_) => {
                let previous = self.staging_dir.join("old").join(index.to_string());
                fs::rename(&swap.target, &previous)
                    .map_err(InstallError::io("move aside", &swap.target))?;
                swap.previous = Some(previous);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(InstallError::io("inspect", &swap.target)(source)),
        }
        if change.content.is_some() {
            if let Some(parent) = swap.target.parent() {
                fs::create_dir_all(parent).map_err(InstallError::io("create", parent))?;
            }
            let staged = self.staged_path(index);
            fs::rename(&staged, &swap.target)
                .map_err(InstallError::io("move into place", &swap.target))?;
            swap.placed_from = Some(staged);
        }
        Ok(())
    }

    fn roll_back(&self, swaps: Vec<Swap>, cause: InstallError) -> InstallError {
        let mut failures = Vec::new();
        for swap in swaps.into_iter().rev() {
            if let Some(staged) = &swap.placed_from
                && let Err(err) = fs::rename(&swap.target, staged)
            {
                failures.push((swap.target, err));
                continue;
            }
            if let Some(previous) = &swap.previous
                && let Err(err) = fs::rename(previous, &swap.target)
            {
                failures.push((swap.target, err));
            }
        }
        if failures.is_empty() {
            self.discard();
            cause
        } else {
            InstallError::RollbackFailed {
                cause: Box::new(cause),
                failures,
                staging_dir: self.staging_dir.clone(),
            }
        }
    }

    /// Removes the staging folder, and the install folder if staging created it and
    /// it is empty again. Runs only on a path that already returns an error, so a
    /// failure here leaves a leftover folder but does not replace that error.
    fn discard(&self) {
        let _ = fs::remove_dir_all(&self.staging_dir);
        if self.created_install_dir {
            let _ = fs::remove_dir(&self.install_dir);
        }
    }

    fn staged_path(&self, index: usize) -> PathBuf {
        self.staging_dir.join("new").join(index.to_string())
    }
}
