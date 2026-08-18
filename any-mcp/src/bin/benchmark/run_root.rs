// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

use std::{fs::File, io::Write, path::Path};

#[cfg(target_os = "linux")]
use std::{
    ffi::CString,
    os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
    path::PathBuf,
};

const MARKER: &str = ".any-mcp-benchmark-run-v1";
const MARKER_PREFIX: &str = "any-mcp-benchmark protected run root v1 ";

pub struct ProtectedRunRoot {
    #[cfg(target_os = "linux")]
    path: PathBuf,
    #[cfg(target_os = "linux")]
    directory: OwnedFd,
    #[cfg(target_os = "linux")]
    nonce: String,
}

impl ProtectedRunRoot {
    #[cfg(target_os = "linux")]
    pub fn open(path: &Path) -> Result<Self, String> {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        if std::env::var("ANY_MCP_BENCHMARK_SUPERVISOR").as_deref() != Ok("systemd-cgroup-netns-v1")
        {
            return Err("live benchmark requires the protected supervisor marker".to_owned());
        }
        let cgroup = std::fs::read_to_string("/proc/self/cgroup")
            .map_err(|_| "cannot inspect benchmark cgroup".to_owned())?;
        if !cgroup.contains("/any-mcp-benchmark-") || !cgroup.contains(".service") {
            return Err("live benchmark is outside its dedicated systemd service".to_owned());
        }
        let nonce = std::env::var("ANY_MCP_BENCHMARK_RUN_NONCE")
            .map_err(|_| "benchmark run nonce is absent".to_owned())?;
        validate_nonce(&nonce)?;
        let canonical = path
            .canonicalize()
            .map_err(|_| "cannot canonicalize benchmark run root".to_owned())?;
        if canonical != path || !canonical.is_absolute() {
            return Err("benchmark run root must be an absolute canonical path".to_owned());
        }
        let metadata = std::fs::symlink_metadata(&canonical)
            .map_err(|_| "cannot inspect benchmark run root".to_owned())?;
        if !metadata.file_type().is_dir()
            || metadata.permissions().mode() & 0o777 != 0o700
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err("benchmark run root must be an owned 0700 directory".to_owned());
        }
        let encoded = CString::new(canonical.as_os_str().as_encoded_bytes())
            .map_err(|_| "benchmark run root contains NUL".to_owned())?;
        // SAFETY: encoded is NUL-terminated, and a successful descriptor is
        // transferred into OwnedFd exactly once.
        let raw = unsafe {
            libc::open(
                encoded.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if raw < 0 {
            return Err("cannot open benchmark run root without following links".to_owned());
        }
        // SAFETY: raw is a new descriptor returned by open.
        let directory = unsafe { OwnedFd::from_raw_fd(raw) };
        let opened = std::fs::File::from(
            directory
                .try_clone()
                .map_err(|_| "cannot duplicate benchmark run-root descriptor".to_owned())?,
        );
        let opened_metadata = opened
            .metadata()
            .map_err(|_| "cannot inspect opened benchmark run root".to_owned())?;
        if opened_metadata.dev() != metadata.dev()
            || opened_metadata.ino() != metadata.ino()
            || opened_metadata.uid() != unsafe { libc::geteuid() }
            || opened_metadata.permissions().mode() & 0o777 != 0o700
            || !opened_metadata.is_dir()
        {
            return Err("benchmark run root changed while it was opened".to_owned());
        }
        let root = Self {
            path: canonical,
            directory,
            nonce,
        };
        let marker = root.read_relative(MARKER, 256)?;
        validate_marker(&marker, &root.nonce)?;
        Ok(root)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn open(_path: &Path) -> Result<Self, String> {
        Err("live benchmarks require the protected Linux supervisor".to_owned())
    }

    #[cfg(target_os = "linux")]
    pub fn create_result(&self, name: &str) -> Result<File, String> {
        validate_name(name)?;
        let encoded = CString::new(name).map_err(|_| "result file contains NUL".to_owned())?;
        // SAFETY: the directory descriptor is live and encoded is a single
        // validated component. O_EXCL prevents replacing an existing result.
        let raw = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                encoded.as_ptr(),
                libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_APPEND,
                0o600,
            )
        };
        if raw < 0 {
            return Err("cannot create append-only benchmark result".to_owned());
        }
        // SAFETY: raw is a new descriptor returned by openat.
        Ok(unsafe { File::from_raw_fd(raw) })
    }

    pub fn append_json<T: serde::Serialize>(file: &mut File, value: &T) -> Result<(), String> {
        serde_json::to_writer(&mut *file, value)
            .map_err(|error| format!("cannot encode benchmark event: {error}"))?;
        file.write_all(b"\n")
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_data())
            .map_err(|error| format!("cannot append benchmark event: {error}"))
    }

    #[cfg(target_os = "linux")]
    pub fn cleanup_arm_files(&self) -> Result<(), String> {
        let directory = std::fs::read_dir(format!("/proc/self/fd/{}", self.directory.as_raw_fd()))
            .map_err(|_| "cannot enumerate benchmark run root".to_owned())?;
        for entry in directory {
            let entry = entry.map_err(|_| "cannot enumerate benchmark run root".to_owned())?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| "benchmark run root contains a non-UTF-8 entry".to_owned())?;
            if !owned_arm_name(&self.nonce, name) {
                continue;
            }
            validate_name(name)?;
            let encoded = CString::new(name).map_err(|_| "arm file contains NUL".to_owned())?;
            let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: status points to writable storage, the directory is
            // live, and AT_SYMLINK_NOFOLLOW closes the replacement race.
            let inspected = unsafe {
                libc::fstatat(
                    self.directory.as_raw_fd(),
                    encoded.as_ptr(),
                    status.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if inspected != 0 {
                return Err("cannot inspect owned benchmark arm file".to_owned());
            }
            // SAFETY: successful fstatat initialized status.
            let status = unsafe { status.assume_init() };
            if status.st_mode & libc::S_IFMT != libc::S_IFREG
                || status.st_mode & 0o777 != 0o600
                || status.st_uid != unsafe { libc::geteuid() }
                || status.st_nlink != 1
            {
                return Err("owned benchmark arm entry has unsafe metadata".to_owned());
            }
            // SAFETY: the name is a validated single component and unlinkat
            // remains anchored to the already verified directory descriptor.
            let removed =
                unsafe { libc::unlinkat(self.directory.as_raw_fd(), encoded.as_ptr(), 0) };
            if removed != 0 {
                return Err("cannot remove benchmark arm scratch file".to_owned());
            }
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(target_os = "linux")]
    fn read_relative(&self, name: &str, limit: usize) -> Result<Vec<u8>, String> {
        use std::io::Read as _;

        validate_name(name)?;
        let encoded = CString::new(name).map_err(|_| "run-root entry contains NUL".to_owned())?;
        // SAFETY: the name is a validated single component and the descriptor
        // is transferred into File exactly once.
        let raw = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                encoded.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if raw < 0 {
            return Err("cannot open benchmark run-root entry".to_owned());
        }
        // SAFETY: raw is a new descriptor returned by openat.
        let file = unsafe { File::from_raw_fd(raw) };
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
            let metadata = file
                .metadata()
                .map_err(|_| "cannot inspect benchmark run-root entry".to_owned())?;
            if !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o777 != 0o600
                || metadata.nlink() != 1
            {
                return Err("benchmark run-root entry has unsafe metadata".to_owned());
            }
        }
        let mut bytes = Vec::new();
        file.take(u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read benchmark run-root entry".to_owned())?;
        if bytes.len() > limit {
            return Err("benchmark run-root entry exceeds its bound".to_owned());
        }
        Ok(bytes)
    }
}

#[cfg(target_os = "linux")]
fn validate_name(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    if name.is_empty()
        || name.len() > 120
        || path.is_absolute()
        || path.components().count() != 1
        || name == "."
        || name == ".."
        || name.contains('/')
    {
        return Err("run-root name must be one safe relative component".to_owned());
    }
    Ok(())
}

fn validate_nonce(nonce: &str) -> Result<(), String> {
    if nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("benchmark run nonce is invalid".to_owned());
    }
    Ok(())
}

fn owned_arm_name(nonce: &str, name: &str) -> bool {
    name.strip_prefix("arm-")
        .and_then(|value| value.strip_prefix(nonce))
        .is_some_and(|value| value.starts_with('-'))
}

fn validate_marker(marker: &[u8], nonce: &str) -> Result<(), String> {
    let expected = format!("{MARKER_PREFIX}{nonce}\n");
    if marker != expected.as_bytes() {
        return Err("benchmark run root marker is absent or invalid".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_correlates_only_owned_arm_names() {
        let nonce = "0123456789abcdef0123456789abcdef";
        validate_nonce(nonce).expect("valid nonce");
        assert!(owned_arm_name(nonce, &format!("arm-{nonce}-local.tmp")));
        assert!(!owned_arm_name(nonce, "arm-other-local.tmp"));
        assert!(!owned_arm_name(nonce, &format!("arm-{nonce}")));
        assert!(validate_nonce("short").is_err());
        validate_marker(format!("{MARKER_PREFIX}{nonce}\n").as_bytes(), nonce)
            .expect("matching marker nonce");
        assert!(
            validate_marker(
                format!("{MARKER_PREFIX}{}\n", "f".repeat(32)).as_bytes(),
                nonce
            )
            .is_err()
        );
    }
}
