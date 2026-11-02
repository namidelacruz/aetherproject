//! Streaming multi-algorithm hashing: every selected algorithm is fed from the
//! same buffer, so verification and MHL hashes never require a second read of
//! the source card.

use crate::error::Result;
use crate::profile::ChecksumAlgorithm;
use sha2::Digest;
use std::collections::HashMap;
use std::path::Path;
use tokio::io::AsyncReadExt;
use xxhash_rust::xxh64::Xxh64;

pub const CHUNK_SIZE: usize = 256 * 1024;

enum One {
    Xxh(Box<Xxh64>),
    Sha256(Box<sha2::Sha256>),
    Blake3(Box<blake3::Hasher>),
    Md5(Box<md5::Md5>),
    Sha1(Box<sha1::Sha1>),
}

/// Hashes a stream with several algorithms at once.
pub struct MultiHasher {
    inner: Vec<(ChecksumAlgorithm, One)>,
}

impl MultiHasher {
    pub fn new(algos: &[ChecksumAlgorithm]) -> Self {
        let mut inner = Vec::new();
        for &a in algos {
            if inner.iter().any(|(x, _)| *x == a) {
                continue;
            }
            let h = match a {
                ChecksumAlgorithm::None => continue,
                ChecksumAlgorithm::XxHash64 => One::Xxh(Box::new(Xxh64::new(0))),
                ChecksumAlgorithm::Sha256 => One::Sha256(Box::default()),
                ChecksumAlgorithm::Blake3 => One::Blake3(Box::default()),
                ChecksumAlgorithm::Md5 => One::Md5(Box::default()),
                ChecksumAlgorithm::Sha1 => One::Sha1(Box::default()),
            };
            inner.push((a, h));
        }
        MultiHasher { inner }
    }

    pub fn update(&mut self, chunk: &[u8]) {
        for (_, h) in self.inner.iter_mut() {
            match h {
                One::Xxh(h) => h.update(chunk),
                One::Sha256(h) => h.update(chunk),
                One::Blake3(h) => {
                    h.update(chunk);
                }
                One::Md5(h) => h.update(chunk),
                One::Sha1(h) => h.update(chunk),
            }
        }
    }

    pub fn finalize(self) -> HashMap<ChecksumAlgorithm, String> {
        self.inner
            .into_iter()
            .map(|(a, h)| {
                let hex = match h {
                    One::Xxh(h) => format!("{:016x}", h.digest()),
                    One::Sha256(h) => format!("{:x}", h.finalize()),
                    One::Blake3(h) => h.finalize().to_hex().to_string(),
                    One::Md5(h) => format!("{:x}", h.finalize()),
                    One::Sha1(h) => format!("{:x}", h.finalize()),
                };
                (a, hex)
            })
            .collect()
    }
}

/// Hashes a file with every algorithm in `algos` in one read pass.
pub async fn hash_file(
    path: &Path,
    algos: &[ChecksumAlgorithm],
) -> Result<HashMap<ChecksumAlgorithm, String>> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut h = MultiHasher::new(algos);
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize())
}

pub fn hash_bytes(data: &[u8], algo: ChecksumAlgorithm) -> String {
    let mut h = MultiHasher::new(&[algo]);
    h.update(data);
    h.finalize()
        .remove(&algo)
        .unwrap_or_else(|| "UNVERIFIED".into())
}
