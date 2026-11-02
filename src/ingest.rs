//! Single-pass streaming ingest with in-flight checksumming, multi-destination
//! writes and post-write verification.

use crate::error::AethrCraftError;
use crate::profile::ChecksumAlgorithm;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use xxhash_rust::xxh64::Xxh64;

const CHUNK_SIZE: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct IngestReceipt {
    pub source_file: PathBuf,
    pub written_destinations: Vec<PathBuf>,
    pub bytes_copied: u64,
    pub checksum_algorithm: ChecksumAlgorithm,
    pub digest_hex: String,
}

/// Streaming hasher over whichever algorithm the profile selects.
enum StreamHasher {
    None,
    XxHash64(Box<Xxh64>),
    Sha256(Box<Sha256>),
    Blake3(Box<blake3::Hasher>),
}

impl StreamHasher {
    fn new(algorithm: ChecksumAlgorithm) -> Self {
        match algorithm {
            ChecksumAlgorithm::None => Self::None,
            ChecksumAlgorithm::XxHash64 => Self::XxHash64(Box::new(Xxh64::new(0))),
            ChecksumAlgorithm::Sha256 => Self::Sha256(Box::new(Sha256::new())),
            ChecksumAlgorithm::Blake3 => Self::Blake3(Box::new(blake3::Hasher::new())),
        }
    }

    fn update(&mut self, chunk: &[u8]) {
        match self {
            Self::None => {}
            Self::XxHash64(h) => h.update(chunk),
            Self::Sha256(h) => h.update(chunk),
            Self::Blake3(h) => {
                h.update(chunk);
            }
        }
    }

    fn finalize_hex(self) -> String {
        match self {
            Self::None => String::from("UNVERIFIED"),
            Self::XxHash64(h) => format!("{:016x}", h.digest()),
            Self::Sha256(h) => format!("{:x}", h.finalize()),
            Self::Blake3(h) => h.finalize().to_hex().to_string(),
        }
    }
}

pub struct SinglePassIngestEngine;

impl SinglePassIngestEngine {
    /// Reads the source once in 256 KB chunks, hashes in flight, writes every
    /// chunk to all destinations, then re-reads each destination to verify.
    pub async fn offload_asset(
        source: &Path,
        destinations: &[PathBuf],
        algorithm: ChecksumAlgorithm,
    ) -> Result<IngestReceipt, AethrCraftError> {
        let mut src_file = tokio::fs::File::open(source).await?;
        let mut dest_files = Vec::with_capacity(destinations.len());

        for dest_path in destinations {
            if let Some(parent) = dest_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(dest_path)
                .await?;
            dest_files.push(file);
        }

        let mut buffer = vec![0u8; CHUNK_SIZE];
        let mut total_bytes = 0u64;
        let mut hasher = StreamHasher::new(algorithm);

        loop {
            let n = src_file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            let chunk = &buffer[..n];
            total_bytes += n as u64;
            hasher.update(chunk);
            for dest in dest_files.iter_mut() {
                dest.write_all(chunk).await?;
            }
        }

        for dest in dest_files.iter_mut() {
            dest.flush().await?;
            dest.sync_all().await?;
        }
        drop(dest_files);

        let source_digest = hasher.finalize_hex();

        if algorithm != ChecksumAlgorithm::None {
            for dest_path in destinations {
                let actual = Self::compute_file_digest(dest_path, algorithm).await?;
                if actual != source_digest {
                    return Err(AethrCraftError::ChecksumMismatch {
                        path: dest_path.clone(),
                        expected: source_digest,
                        actual,
                    });
                }
            }
        }

        Ok(IngestReceipt {
            source_file: source.to_path_buf(),
            written_destinations: destinations.to_vec(),
            bytes_copied: total_bytes,
            checksum_algorithm: algorithm,
            digest_hex: source_digest,
        })
    }

    /// Computes the digest of a file on disk with the given algorithm.
    pub async fn compute_file_digest(
        path: &Path,
        algorithm: ChecksumAlgorithm,
    ) -> Result<String, AethrCraftError> {
        let mut file = tokio::fs::File::open(path).await?;
        let mut buffer = vec![0u8; CHUNK_SIZE];
        let mut hasher = StreamHasher::new(algorithm);
        loop {
            let n = file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        Ok(hasher.finalize_hex())
    }
}
