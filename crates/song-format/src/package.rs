use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, Write};
use thiserror::Error;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

#[derive(Debug, Error)]
pub enum PackageError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Invalid path in archive: '{0}' contains prohibited sequence")]
    SecurityPathViolation(String),
    #[error("Exceeded archive entries limit: {0} > 256")]
    EntryLimitExceeded(usize),
    #[error("Exceeded uncompressed size limit: {0} > 2GiB")]
    SizeLimitExceeded(u64),
    #[error("Manifest validation failed: {0}")]
    InvalidManifest(String),
    #[error("Hash mismatch for '{path}': expected {expected}, computed {computed}")]
    HashMismatch {
        path: String,
        expected: String,
        computed: String,
    },
    #[error("Required file missing from package: '{0}'")]
    MissingRequiredFile(String),
    #[error("Duplicate archive entry: '{0}'")]
    DuplicateEntry(String),
    #[error("Archive entry is not declared exactly once in manifest: '{0}'")]
    UnlistedEntry(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileRole {
    Chart,
    Backing,
    Guide,
    Jacket,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestFileEntry {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub role: FileRole,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageManifest {
    pub package_version: String,
    pub package_id: String,
    pub chart_path: String,
    pub created_by: String,
    pub files: Vec<ManifestFileEntry>,
}

pub struct PackageReader;

impl PackageReader {
    /// Validates a zip package path against §11.2 security rules
    pub fn validate_entry_path(path: &str) -> Result<(), PackageError> {
        // Must not contain ..
        if path.contains("..") {
            return Err(PackageError::SecurityPathViolation(path.to_string()));
        }
        // Must not start with / or \ or drive letter (e.g. C:)
        if path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
            return Err(PackageError::SecurityPathViolation(path.to_string()));
        }
        // Check Windows reserved names
        let reserved = [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "LPT1", "LPT2",
        ];
        let parts: Vec<&str> = path.split(&['/', '\\'][..]).collect();
        for p in parts {
            let stem = p.split('.').next().unwrap_or("").to_ascii_uppercase();
            if reserved.contains(&stem.as_str()) {
                return Err(PackageError::SecurityPathViolation(path.to_string()));
            }
            if p.ends_with('.') || p.ends_with(' ') {
                return Err(PackageError::SecurityPathViolation(path.to_string()));
            }
        }
        Ok(())
    }

    /// Reads and inspects a .kpk package
    pub fn inspect_package<R: Read + Seek>(
        reader: R,
    ) -> Result<(PackageManifest, String), PackageError> {
        let mut archive = ZipArchive::new(reader)?;

        if archive.len() > 256 {
            return Err(PackageError::EntryLimitExceeded(archive.len()));
        }

        let mut total_uncompressed_bytes: u64 = 0;
        let max_total_bytes: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

        let mut manifest_bytes: Option<Vec<u8>> = None;
        let mut file_hashes = HashMap::new();
        let mut normalized_names = HashSet::new();

        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let raw_name = file.name().to_string();
            Self::validate_entry_path(&raw_name)?;
            let normalized = raw_name.replace('\\', "/").to_ascii_lowercase();
            if !normalized_names.insert(normalized) {
                return Err(PackageError::DuplicateEntry(raw_name));
            }

            total_uncompressed_bytes += file.size();
            if total_uncompressed_bytes > max_total_bytes {
                return Err(PackageError::SizeLimitExceeded(total_uncompressed_bytes));
            }

            let mut contents = Vec::new();
            file.read_to_end(&mut contents)?;

            let mut hasher = Sha256::new();
            hasher.update(&contents);
            let hash_hex = format!("{:x}", hasher.finalize());
            file_hashes.insert(raw_name.clone(), (contents.len() as u64, hash_hex));

            if raw_name == "manifest.json" {
                if contents.len() > 32 * 1024 * 1024 {
                    return Err(PackageError::SizeLimitExceeded(contents.len() as u64));
                }
                manifest_bytes = Some(contents);
            }
        }

        let manifest_data = manifest_bytes
            .ok_or_else(|| PackageError::MissingRequiredFile("manifest.json".to_string()))?;

        let manifest: PackageManifest = serde_json::from_slice(&manifest_data)?;
        if manifest.package_version != "1.0.0" {
            return Err(PackageError::InvalidManifest(format!(
                "Invalid packageVersion: {} (expected 1.0.0)",
                manifest.package_version
            )));
        }
        if manifest.package_id.is_empty()
            || manifest.package_id.len() > 96
            || !manifest
                .package_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(PackageError::InvalidManifest(
                "packageId must match [A-Za-z0-9_-]{1,96}".to_string(),
            ));
        }
        if manifest.chart_path != "chart.json" {
            return Err(PackageError::InvalidManifest(
                "chartPath must be chart.json in packageVersion 1.0.0".to_string(),
            ));
        }

        let mut declared_paths = HashSet::new();
        let mut chart_count = 0;
        let mut backing_count = 0;

        // Verify manifest file entries
        for entry in &manifest.files {
            Self::validate_entry_path(&entry.path)?;
            let normalized = entry.path.replace('\\', "/").to_ascii_lowercase();
            if !declared_paths.insert(normalized) {
                return Err(PackageError::DuplicateEntry(entry.path.clone()));
            }
            if entry.role == FileRole::Chart {
                chart_count += 1;
                if entry.path != manifest.chart_path {
                    return Err(PackageError::InvalidManifest(
                        "chart role must point to chartPath".to_string(),
                    ));
                }
            }
            if entry.role == FileRole::Backing {
                backing_count += 1;
            }
            let actual = file_hashes
                .get(&entry.path)
                .ok_or_else(|| PackageError::MissingRequiredFile(entry.path.clone()))?;

            if actual.0 != entry.bytes {
                return Err(PackageError::InvalidManifest(format!(
                    "Size mismatch for '{}': manifest says {}, actual is {}",
                    entry.path, entry.bytes, actual.0
                )));
            }

            if actual.1.to_lowercase() != entry.sha256.to_lowercase() {
                return Err(PackageError::HashMismatch {
                    path: entry.path.clone(),
                    expected: entry.sha256.clone(),
                    computed: actual.1.clone(),
                });
            }
        }

        if chart_count != 1 || backing_count != 1 {
            return Err(PackageError::InvalidManifest(format!(
                "expected exactly one chart and one backing file, found chart={chart_count}, backing={backing_count}"
            )));
        }
        for path in file_hashes
            .keys()
            .filter(|path| path.as_str() != "manifest.json")
        {
            if !declared_paths.contains(&path.replace('\\', "/").to_ascii_lowercase()) {
                return Err(PackageError::UnlistedEntry(path.clone()));
            }
        }
        if manifest.files.len() + 1 != file_hashes.len() {
            return Err(PackageError::InvalidManifest(
                "manifest files must list every non-manifest entry exactly once".to_string(),
            ));
        }

        let chart_data = Self::extract_file_from_archive(&mut archive, &manifest.chart_path)?;
        if chart_data.len() > 32 * 1024 * 1024 {
            return Err(PackageError::SizeLimitExceeded(chart_data.len() as u64));
        }
        let chart_json_str = String::from_utf8(chart_data)
            .map_err(|e| PackageError::InvalidManifest(e.to_string()))?;

        Ok((manifest, chart_json_str))
    }

    pub fn extract_file<R: Read + Seek>(reader: R, path: &str) -> Result<Vec<u8>, PackageError> {
        Self::validate_entry_path(path)?;
        let mut archive = ZipArchive::new(reader)?;
        Self::extract_file_from_archive(&mut archive, path)
    }

    fn extract_file_from_archive<R: Read + Seek>(
        archive: &mut ZipArchive<R>,
        path: &str,
    ) -> Result<Vec<u8>, PackageError> {
        let mut file = archive
            .by_name(path)
            .map_err(|_| PackageError::MissingRequiredFile(path.to_string()))?;
        let mut contents = Vec::with_capacity(file.size().min(32 * 1024 * 1024) as usize);
        file.read_to_end(&mut contents)?;
        Ok(contents)
    }
}

pub struct PackageWriter;

impl PackageWriter {
    pub fn write_package<W: Write + Seek>(
        writer: W,
        package_id: &str,
        created_by: &str,
        chart_bytes: &[u8],
        backing_path: &str,
        backing_bytes: &[u8],
    ) -> Result<W, PackageError> {
        PackageReader::validate_entry_path(backing_path)?;
        if backing_path == "manifest.json" || backing_path == "chart.json" {
            return Err(PackageError::InvalidManifest(
                "backing path collides with a required package file".to_string(),
            ));
        }
        let manifest = PackageManifest {
            package_version: "1.0.0".to_string(),
            package_id: package_id.to_string(),
            chart_path: "chart.json".to_string(),
            created_by: created_by.to_string(),
            files: vec![
                Self::manifest_entry("chart.json", chart_bytes, FileRole::Chart),
                Self::manifest_entry(backing_path, backing_bytes, FileRole::Backing),
            ],
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let mut zip = ZipWriter::new(writer);
        zip.start_file("manifest.json", options)?;
        zip.write_all(&manifest_bytes)?;
        zip.start_file("chart.json", options)?;
        zip.write_all(chart_bytes)?;
        zip.start_file(backing_path, options)?;
        zip.write_all(backing_bytes)?;
        Ok(zip.finish()?)
    }

    fn manifest_entry(path: &str, contents: &[u8], role: FileRole) -> ManifestFileEntry {
        let mut hasher = Sha256::new();
        hasher.update(contents);
        ManifestFileEntry {
            path: path.to_string(),
            bytes: contents.len() as u64,
            sha256: format!("{:x}", hasher.finalize()),
            role,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn package_writer_reader_roundtrip_preserves_chart_and_backing() {
        let chart = br#"{"schemaVersion":"2.0.0"}"#;
        let backing = b"test backing bytes";
        let cursor = PackageWriter::write_package(
            Cursor::new(Vec::new()),
            "test_song",
            "test",
            chart,
            "media/backing.ogg",
            backing,
        )
        .unwrap();
        let bytes = cursor.into_inner();
        let (manifest, chart_json) = PackageReader::inspect_package(Cursor::new(&bytes)).unwrap();
        assert_eq!(manifest.package_id, "test_song");
        assert_eq!(chart_json.as_bytes(), chart);
        assert_eq!(
            PackageReader::extract_file(Cursor::new(&bytes), "media/backing.ogg").unwrap(),
            backing
        );
    }

    #[test]
    fn reader_rejects_unlisted_archive_entries() {
        let cursor = PackageWriter::write_package(
            Cursor::new(Vec::new()),
            "test_song",
            "test",
            b"{}",
            "media/backing.ogg",
            b"audio",
        )
        .unwrap();
        let mut zip = ZipWriter::new_append(cursor).unwrap();
        zip.start_file("undeclared.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"not in manifest").unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert!(matches!(
            PackageReader::inspect_package(Cursor::new(bytes)),
            Err(PackageError::UnlistedEntry(_))
        ));
    }
}
