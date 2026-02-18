use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use zip::ZipArchive;

#[derive(Debug, Clone, Serialize)]
pub struct ArchiveFileEntry {
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveSourceKind {
    Directory,
    Zip,
}

impl ArchiveSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::Zip => "zip",
        }
    }
}

#[derive(Clone)]
pub struct ArchiveReader {
    root: PathBuf,
    source: ArchiveSourceKind,
    zip: Option<ZipReaderState>,
}

#[derive(Clone)]
struct ZipReaderState {
    archive: Arc<Mutex<ZipArchive<fs::File>>>,
    files: Arc<Vec<ArchiveFileEntry>>,
}

impl std::fmt::Debug for ArchiveReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveReader")
            .field("root", &self.root)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl ArchiveReader {
    pub fn from_path(path: &Path) -> Result<Self> {
        if path.is_dir() {
            return Ok(Self {
                root: path.to_path_buf(),
                source: ArchiveSourceKind::Directory,
                zip: None,
            });
        }
        if path.is_file() {
            let file = fs::File::open(path)
                .with_context(|| format!("failed to open archive file {}", path.display()))?;
            if let Ok(mut zip) = ZipArchive::new(file) {
                let mut files = Vec::new();
                for idx in 0..zip.len() {
                    let entry = zip.by_index(idx)?;
                    if entry.is_dir() {
                        continue;
                    }
                    files.push(ArchiveFileEntry {
                        path: entry.name().to_string(),
                        bytes: entry.size(),
                    });
                }
                files.sort_by(|a, b| a.path.cmp(&b.path));
                return Ok(Self {
                    root: path.to_path_buf(),
                    source: ArchiveSourceKind::Zip,
                    zip: Some(ZipReaderState {
                        archive: Arc::new(Mutex::new(zip)),
                        files: Arc::new(files),
                    }),
                });
            }
        }
        Err(anyhow!(
            "archive must be a directory or zip file: {}",
            path.display()
        ))
    }

    pub fn source(&self) -> ArchiveSourceKind {
        self.source
    }

    pub fn list_files(&self) -> Result<Vec<ArchiveFileEntry>> {
        match self.source {
            ArchiveSourceKind::Directory => {
                let mut entries = Vec::new();
                let mut stack = vec![self.root.clone()];
                while let Some(dir) = stack.pop() {
                    for entry in fs::read_dir(&dir)? {
                        let entry = entry?;
                        let path = entry.path();
                        if path.is_dir() {
                            stack.push(path);
                            continue;
                        }
                        let rel = path.strip_prefix(&self.root).with_context(|| {
                            format!("archive file not under root: {}", path.display())
                        })?;
                        let meta = entry.metadata()?;
                        entries.push(ArchiveFileEntry {
                            path: rel.to_string_lossy().into_owned(),
                            bytes: meta.len(),
                        });
                    }
                }
                entries.sort_by(|a, b| a.path.cmp(&b.path));
                Ok(entries)
            }
            ArchiveSourceKind::Zip => {
                let state = self.zip_state()?;
                Ok(state.files.as_ref().clone())
            }
        }
    }

    pub fn read_bytes(&self, rel_path: &str) -> Result<Vec<u8>> {
        match self.source {
            ArchiveSourceKind::Directory => {
                let path = self.root.join(rel_path);
                fs::read(&path)
                    .with_context(|| format!("failed to read archive file {}", path.display()))
            }
            ArchiveSourceKind::Zip => {
                let state = self.zip_state()?;
                let mut zip = state
                    .archive
                    .lock()
                    .map_err(|_| anyhow!("zip archive lock poisoned"))?;
                let mut entry = zip
                    .by_name(rel_path)
                    .with_context(|| format!("archive entry not found in zip: {rel_path}"))?;
                let mut out = Vec::new();
                entry.read_to_end(&mut out)?;
                drop(entry);
                drop(zip);
                Ok(out)
            }
        }
    }

    pub fn read_bytes_if_exists(&self, rel_path: &str) -> Result<Option<Vec<u8>>> {
        match self.source {
            ArchiveSourceKind::Directory => {
                let path = self.root.join(rel_path);
                if !path.is_file() {
                    return Ok(None);
                }
                let bytes = fs::read(&path)
                    .with_context(|| format!("failed to read archive file {}", path.display()))?;
                Ok(Some(bytes))
            }
            ArchiveSourceKind::Zip => {
                let state = self.zip_state()?;
                let mut zip = state
                    .archive
                    .lock()
                    .map_err(|_| anyhow!("zip archive lock poisoned"))?;
                let Ok(mut entry) = zip.by_name(rel_path) else {
                    return Ok(None);
                };
                let mut out = Vec::new();
                entry.read_to_end(&mut out)?;
                drop(entry);
                drop(zip);
                Ok(Some(out))
            }
        }
    }

    fn zip_state(&self) -> Result<&ZipReaderState> {
        self.zip
            .as_ref()
            .ok_or_else(|| anyhow!("zip archive state unavailable"))
    }
}

fn looks_like_content_id(value: &str) -> bool {
    value.len() >= 20
        && value.len() <= 128
        && value.starts_with("bafy")
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
}

pub fn infer_object_id_from_snapshot_path(path: &str) -> Option<String> {
    let file_name = Path::new(path).file_name().and_then(|s| s.to_str())?;
    let stem = if let Some(stem) = file_name.strip_suffix(".pb.json") {
        stem
    } else if let Some(stem) = file_name.strip_suffix(".pb") {
        stem
    } else {
        return None;
    };
    looks_like_content_id(stem).then(|| stem.to_string())
}

pub fn infer_object_ids_from_files(files: &[ArchiveFileEntry]) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for entry in files {
        let path = Path::new(&entry.path);
        let under_objects = path
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .is_some_and(|root| root == "objects");
        if !under_objects {
            continue;
        }
        if let Some(id) = infer_object_id_from_snapshot_path(&entry.path) {
            ids.insert(id);
        }
    }
    ids.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn reader_lists_and_reads_directory_archive() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("objects")).unwrap();
        fs::write(temp.path().join("manifest.json"), b"{}").unwrap();
        fs::write(temp.path().join("objects/obj.pb"), b"payload").unwrap();

        let reader = ArchiveReader::from_path(temp.path()).unwrap();
        assert_eq!(reader.source(), ArchiveSourceKind::Directory);

        let files = reader.list_files().unwrap();
        assert!(files.iter().any(|entry| entry.path == "manifest.json"));
        assert!(files.iter().any(|entry| entry.path == "objects/obj.pb"));
        assert_eq!(reader.read_bytes("objects/obj.pb").unwrap(), b"payload");
    }

    #[test]
    fn reader_lists_and_reads_zip_archive() {
        let temp = tempfile::tempdir().unwrap();
        let zip_path = temp.path().join("archive.zip");
        let file = fs::File::create(&zip_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"{}").unwrap();
        writer
            .start_file("objects/obj.pb", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"payload").unwrap();
        writer.finish().unwrap();

        let reader = ArchiveReader::from_path(&zip_path).unwrap();
        assert_eq!(reader.source(), ArchiveSourceKind::Zip);

        let files = reader.list_files().unwrap();
        assert!(files.iter().any(|entry| entry.path == "manifest.json"));
        assert!(files.iter().any(|entry| entry.path == "objects/obj.pb"));
        assert_eq!(reader.read_bytes("objects/obj.pb").unwrap(), b"payload");
    }

    #[test]
    fn infer_object_id_accepts_bafy_id_stems() {
        let id = "bafyreiaebddr63d7sye3eggmtkyeioqxftoaipobsynceksj6faedvd2xi";
        let path = format!("objects/{id}.pb");
        assert_eq!(
            infer_object_id_from_snapshot_path(&path),
            Some(id.to_string())
        );
    }
}
