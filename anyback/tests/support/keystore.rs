use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use tempfile::{Builder, TempDir};

/// A command-scoped private clone of a file-backed Anytype keystore.
///
/// The temporary directory owns the database and any SQLite sidecars. Dropping
/// this value removes the complete clone after the child command exits.
pub(crate) struct PrivateKeystore {
    database: PathBuf,
    _directory: TempDir,
}

impl PrivateKeystore {
    fn clone_from(source_db: &Path) -> Result<Self> {
        if !source_db.is_file() {
            bail!("source keystore does not exist: {}", source_db.display());
        }

        let _directory = Builder::new()
            .prefix("anyback-keystore-")
            .tempdir()
            .context("failed to create private keystore directory")?;
        set_directory_private(_directory.path())?;

        let database = _directory.path().join("keystore.db");
        copy_private(source_db, &database)?;
        for suffix in ["-wal", "-shm"] {
            let source_sidecar = PathBuf::from(format!("{}{}", source_db.display(), suffix));
            if source_sidecar.is_file() {
                let target_sidecar = PathBuf::from(format!("{}{}", database.display(), suffix));
                copy_private(&source_sidecar, &target_sidecar)?;
            }
        }

        Ok(Self {
            database,
            _directory,
        })
    }

    fn uri(&self) -> String {
        format!("file:path={}", self.database.display())
    }
}

/// Configures `command` with an isolated clone when the active keystore is
/// file-backed. The returned guard must live until the command exits.
pub(crate) fn configure_test_keystore(command: &mut Command) -> Result<Option<PrivateKeystore>> {
    let Some(source) = std::env::var("ANYTYPE_KEYSTORE")
        .ok()
        .and_then(|value| value.strip_prefix("file:path=").map(ToString::to_string))
    else {
        return Ok(None);
    };

    let keystore = PrivateKeystore::clone_from(Path::new(&source))?;
    command.env("ANYTYPE_KEYSTORE", keystore.uri());
    Ok(Some(keystore))
}

fn copy_private(source: &Path, target: &Path) -> Result<()> {
    fs::copy(source, target).with_context(|| {
        format!(
            "failed to copy keystore file {} to {}",
            source.display(),
            target.display()
        )
    })?;
    set_file_private(target)
}

#[cfg(unix)]
fn set_directory_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to protect directory {}", path.display()))
}

#[cfg(not(unix))]
fn set_directory_private(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_file_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to protect keystore file {}", path.display()))
}

#[cfg(not(unix))]
fn set_file_private(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn clone_is_private_and_removed_on_drop() -> Result<()> {
        let source_dir = tempfile::tempdir()?;
        let source = source_dir.path().join("source.db");
        fs::write(&source, b"database")?;
        fs::write(PathBuf::from(format!("{}-wal", source.display())), b"wal")?;
        fs::write(PathBuf::from(format!("{}-shm", source.display())), b"shm")?;
        for path in [
            source.clone(),
            PathBuf::from(format!("{}-wal", source.display())),
            PathBuf::from(format!("{}-shm", source.display())),
        ] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
        }

        let clone = PrivateKeystore::clone_from(&source)?;
        let clone_dir = clone._directory.path().to_path_buf();
        let clone_paths = [
            clone.database.clone(),
            PathBuf::from(format!("{}-wal", clone.database.display())),
            PathBuf::from(format!("{}-shm", clone.database.display())),
        ];

        assert_eq!(
            fs::metadata(&clone_dir)?.permissions().mode() & 0o777,
            0o700
        );
        for path in &clone_paths {
            assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
        }

        drop(clone);
        assert!(
            !clone_dir.exists(),
            "private keystore clone was not removed"
        );
        Ok(())
    }
}
