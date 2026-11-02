//! Zero-decode embedded preview extraction for RAW files.

use crate::error::AethrCraftError;
use std::fs::File;
use std::path::Path;

pub struct ZeroDecodePreviewExtractor;

impl ZeroDecodePreviewExtractor {
    /// Memory-maps a RAW file (`.CR3`, `.ARW`, `.NEF`, ...) and returns the
    /// largest embedded JPEG (SOI `FF D8 FF` .. EOI `FF D9`) without
    /// demosaicing sensor data.
    pub fn find_largest_embedded_jpeg(raw_path: &Path) -> Result<Option<Vec<u8>>, AethrCraftError> {
        let file = File::open(raw_path)?;
        if file.metadata()?.len() == 0 {
            return Ok(None);
        }
        // SAFETY: the mapping is read-only and dropped before returning; the
        // caller must not truncate the file concurrently.
        let mmap = unsafe { memmap2::MmapOptions::new().map(&file)? };
        Ok(Self::find_largest_jpeg_in(&mmap).map(<[u8]>::to_vec))
    }

    /// Scans a byte slice for the largest SOI..EOI JPEG segment.
    pub fn find_largest_jpeg_in(bytes: &[u8]) -> Option<&[u8]> {
        let mut best: Option<&[u8]> = None;
        let mut i = 0;

        while i + 3 < bytes.len() {
            if bytes[i] == 0xFF && bytes[i + 1] == 0xD8 && bytes[i + 2] == 0xFF {
                let start = i;
                let eoi = (start + 3..bytes.len().saturating_sub(1))
                    .find(|&e| bytes[e] == 0xFF && bytes[e + 1] == 0xD9);
                match eoi {
                    Some(end) => {
                        let candidate = &bytes[start..end + 2];
                        if best.is_none_or(|b| candidate.len() > b.len()) {
                            best = Some(candidate);
                        }
                        i = end + 2;
                        continue;
                    }
                    // No EOI anywhere after this point: nothing further can match.
                    None => break,
                }
            }
            i += 1;
        }
        best
    }
}
