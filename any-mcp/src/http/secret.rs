// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Static bearer token loading with redaction and constant-time comparison.
//!
//! The configured token represents one fixed principal for local debugging.
//! It is held in zeroizing memory, never implements `Debug` or `Display`, and
//! is compared through fixed-length digests in constant time so a request
//! cannot measure a prefix match.

use std::{fmt, fs, path::Path};

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

const MIN_TOKEN_BYTES: usize = 43;
const MAX_TOKEN_BYTES: usize = 512;
/// One token, one optional newline, and slack to detect oversized files
/// without buffering them.
const MAX_FILE_BYTES: u64 = (MAX_TOKEN_BYTES + 2) as u64;

/// One validated static bearer token held in zeroizing memory.
///
/// The token value is reachable only through [`StaticToken::matches`]. The
/// type intentionally implements neither `Debug`, `Display`, `Clone`,
/// serialization, nor accessors returning the raw bytes.
pub struct StaticToken {
    digest: [u8; 32],
    _token: Zeroizing<Vec<u8>>,
}

impl StaticToken {
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self::from_file_bytes(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .expect("static test token")
    }

    /// Loads and validates the token file at the configured absolute path.
    ///
    /// The path must name a regular non-symlink file containing exactly one
    /// 43..512-byte base64url token and at most one trailing newline. On
    /// Unix the file must be owned by the effective user with no group or
    /// other permission bits.
    ///
    /// # Errors
    ///
    /// Returns a fixed [`StaticTokenError`] category. Diagnostics never
    /// include the path or any file content.
    pub fn load(path: &Path) -> Result<Self, StaticTokenError> {
        if !path.is_absolute() {
            return Err(StaticTokenError::NotAbsolute);
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| StaticTokenError::Unreadable)?;
        if metadata.file_type().is_symlink() {
            return Err(StaticTokenError::Symlink);
        }
        if !metadata.file_type().is_file() {
            return Err(StaticTokenError::NotRegularFile);
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(StaticTokenError::Grammar);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // SAFETY: geteuid has no preconditions and cannot fail.
            let effective_uid = unsafe { libc::geteuid() };
            if metadata.uid() != effective_uid {
                return Err(StaticTokenError::Ownership);
            }
            if metadata.mode() & 0o077 != 0 {
                return Err(StaticTokenError::Permissions);
            }
        }
        let raw = Zeroizing::new(fs::read(path).map_err(|_| StaticTokenError::Unreadable)?);
        Self::from_file_bytes(&raw)
    }

    fn from_file_bytes(raw: &[u8]) -> Result<Self, StaticTokenError> {
        let token = raw.strip_suffix(b"\n").unwrap_or(raw);
        if token.len() < MIN_TOKEN_BYTES || token.len() > MAX_TOKEN_BYTES {
            return Err(StaticTokenError::Grammar);
        }
        let base64url = token
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-' || *byte == b'_');
        if !base64url {
            return Err(StaticTokenError::Grammar);
        }
        let token = Zeroizing::new(token.to_vec());
        Ok(Self {
            digest: Sha256::digest(&token).into(),
            _token: token,
        })
    }

    /// Compares a presented credential in constant time.
    ///
    /// Both sides are reduced to fixed-length digests first, so neither the
    /// comparison time nor an early length check reveals token content.
    #[must_use]
    pub fn matches(&self, presented: &[u8]) -> bool {
        let presented: [u8; 32] = Sha256::digest(presented).into();
        self.digest.ct_eq(&presented).into()
    }
}

/// A fixed static token failure category without path or content details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaticTokenError {
    /// The configured path is not absolute.
    NotAbsolute,
    /// The path could not be read.
    Unreadable,
    /// The path names a symlink.
    Symlink,
    /// The path is not a regular file.
    NotRegularFile,
    /// The file is not owned by the effective user.
    Ownership,
    /// The file grants group or other permission bits.
    Permissions,
    /// The content is not one 43..512-byte base64url token.
    Grammar,
}

impl fmt::Display for StaticTokenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::NotAbsolute => "static token path must be absolute",
            Self::Unreadable => "static token file could not be read",
            Self::Symlink => "static token path must not be a symlink",
            Self::NotRegularFile => "static token path must be a regular file",
            Self::Ownership => "static token file must be owned by the effective user",
            Self::Permissions => "static token file must not grant group or other access",
            Self::Grammar => "static token file must hold one 43..512-byte base64url token",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for StaticTokenError {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NAME_COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TempTokenFile {
        path: std::path::PathBuf,
    }

    impl TempTokenFile {
        fn write(content: &[u8]) -> Self {
            let name = format!(
                "any-mcp-token-test-{}-{}",
                std::process::id(),
                NAME_COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(name);
            fs::write(&path, content).expect("write temp token");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                    .expect("restrict temp token");
            }
            Self { path }
        }
    }

    impl Drop for TempTokenFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn valid_token() -> String {
        "a".repeat(43)
    }

    /// `StaticToken` has no `Debug` by design, so `unwrap_err` cannot apply.
    fn load_err(path: &Path) -> StaticTokenError {
        match StaticToken::load(path) {
            Ok(_) => panic!("expected static token rejection"),
            Err(error) => error,
        }
    }

    #[test]
    fn accepts_one_token_with_optional_trailing_newline() {
        let token = valid_token();
        for content in [token.clone(), format!("{token}\n")] {
            let file = TempTokenFile::write(content.as_bytes());
            let loaded = StaticToken::load(&file.path).expect("valid token");
            assert!(loaded.matches(token.as_bytes()));
            assert!(!loaded.matches(b"wrong"));
            assert!(!loaded.matches(format!("{token}\n").as_bytes()));
            assert!(!loaded.matches(&token.as_bytes()[..42]));
        }
    }

    #[test]
    fn accepts_full_base64url_alphabet_and_maximum_length() {
        let token = format!("Az09-_{}", "b".repeat(506));
        assert_eq!(token.len(), 512);
        let file = TempTokenFile::write(token.as_bytes());
        let loaded = StaticToken::load(&file.path).expect("valid token");
        assert!(loaded.matches(token.as_bytes()));
    }

    #[test]
    fn rejects_grammar_violations() {
        for content in [
            "a".repeat(42),
            "a".repeat(513),
            format!("{}\n{}", valid_token(), valid_token()),
            format!("{}\n\n", valid_token()),
            format!("{}=", "a".repeat(42)),
            format!("{} ", "a".repeat(42)),
            format!("{}\r\n", "a".repeat(43)),
            String::new(),
        ] {
            let file = TempTokenFile::write(content.as_bytes());
            assert_eq!(
                load_err(&file.path),
                StaticTokenError::Grammar,
                "content bytes {}",
                content.len()
            );
        }
    }

    #[test]
    fn rejects_relative_and_missing_paths() {
        assert_eq!(
            load_err(Path::new("relative/token")),
            StaticTokenError::NotAbsolute
        );
        assert_eq!(
            load_err(&std::env::temp_dir().join("any-mcp-token-test-missing")),
            StaticTokenError::Unreadable
        );
    }

    #[test]
    fn rejects_directories() {
        let temporary = std::env::temp_dir()
            .canonicalize()
            .expect("canonical temporary directory");
        assert!(matches!(
            load_err(&temporary),
            StaticTokenError::NotRegularFile | StaticTokenError::Permissions
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_and_open_permissions() {
        let target = TempTokenFile::write(valid_token().as_bytes());
        let link = std::env::temp_dir().join(format!(
            "any-mcp-token-test-link-{}-{}",
            std::process::id(),
            NAME_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::os::unix::fs::symlink(&target.path, &link).expect("create symlink");
        assert_eq!(load_err(&link), StaticTokenError::Symlink);
        let _ = fs::remove_file(&link);

        use std::os::unix::fs::PermissionsExt;
        for mode in [0o640, 0o604, 0o660, 0o644] {
            let file = TempTokenFile::write(valid_token().as_bytes());
            fs::set_permissions(&file.path, fs::Permissions::from_mode(mode))
                .expect("widen permissions");
            assert_eq!(
                load_err(&file.path),
                StaticTokenError::Permissions,
                "mode {mode:o}"
            );
        }
    }

    #[test]
    fn error_text_is_fixed_and_content_free() {
        let secret = valid_token();
        let file = TempTokenFile::write(format!("{secret}=").as_bytes());
        let error = load_err(&file.path);
        let message = error.to_string();
        assert!(!message.contains(&secret));
        assert!(!message.contains(file.path.to_str().expect("utf8 path")));
    }
}
