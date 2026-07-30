use reqwest::Certificate;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Default)]
pub struct UpstreamCaConfig {
    configured_path: Option<PathBuf>,
    certificates: Vec<Certificate>,
}

impl UpstreamCaConfig {
    pub fn load(path: Option<&Path>) -> io::Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };

        let metadata = fs::metadata(path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to read upstream CA path {}: {error}",
                    path.display()
                ),
            )
        })?;

        let files = if metadata.is_file() {
            vec![path.to_path_buf()]
        } else if metadata.is_dir() {
            certificate_files(path)?
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "upstream CA path is not a file or directory: {}",
                    path.display()
                ),
            ));
        };

        if files.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "upstream CA directory contains no .crt or .pem certificates: {}",
                    path.display()
                ),
            ));
        }

        let mut certificates = Vec::new();
        for file in files {
            let bytes = fs::read(&file).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "failed to read upstream CA certificate {}: {error}",
                        file.display()
                    ),
                )
            })?;
            let parsed = Certificate::from_pem_bundle(&bytes).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid upstream CA certificate: {}", file.display()),
                )
            })?;
            if parsed.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid upstream CA certificate: {}", file.display()),
                ));
            }
            certificates.extend(parsed);
        }

        Ok(Self {
            configured_path: Some(path.to_path_buf()),
            certificates,
        })
    }

    pub fn certificates(&self) -> &[Certificate] {
        &self.certificates
    }

    pub fn is_configured(&self) -> bool {
        self.configured_path.is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.certificates.is_empty()
    }

    pub fn len(&self) -> usize {
        self.certificates.len()
    }
}

impl fmt::Debug for UpstreamCaConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpstreamCaConfig")
            .field("configured_path", &self.configured_path)
            .field("certificate_count", &self.certificates.len())
            .finish()
    }
}

fn certificate_files(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "failed to read upstream CA directory {}: {error}",
                directory.display()
            ),
        )
    })? {
        let entry = entry.map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to read upstream CA directory entry in {}: {error}",
                    directory.display()
                ),
            )
        })?;
        if !entry
            .file_type()
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "failed to inspect upstream CA path {}: {error}",
                        entry.path().display()
                    ),
                )
            })?
            .is_file()
        {
            continue;
        }
        let extension = entry
            .path()
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(extension.as_str(), "crt" | "pem") {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}
