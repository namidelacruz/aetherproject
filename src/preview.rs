//! Zero-decode embedded preview extraction and preview rendering.
//!
//! Extraction order: (1) JPEG ranges found through IFD / ISOBMFF offsets,
//! (2) a byte scan for SOI markers. Both use a JPEG *marker walk* to find the
//! real end of each image, so an EXIF thumbnail nested inside a large preview's
//! APP1 segment can't truncate it.

use crate::error::{AethrCraftError, Result};
use crate::lut::CubeLut;
use crate::metadata;
use image::{DynamicImage, RgbImage};
use std::fs::File;
use std::path::Path;

/// Returns the exclusive end offset of the JPEG starting at `start` by walking
/// its segment markers and entropy-coded data.
pub fn jpeg_extent(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start..start + 2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = start + 2;
    loop {
        // Skip fill bytes.
        while bytes.get(i)? == &0xFF && bytes.get(i + 1)? == &0xFF {
            i += 1;
        }
        if *bytes.get(i)? != 0xFF {
            return None;
        }
        let marker = *bytes.get(i + 1)?;
        match marker {
            0xD9 => return Some(i + 2),
            0x01 | 0xD0..=0xD7 => {
                i += 2;
                continue;
            }
            _ => {}
        }
        let len = u16::from_be_bytes([*bytes.get(i + 2)?, *bytes.get(i + 3)?]) as usize;
        if len < 2 {
            return None;
        }
        i = i.checked_add(2 + len)?;
        if i > bytes.len() {
            return None;
        }
        if marker == 0xDA {
            // Entropy-coded data: scan for the next real marker.
            loop {
                let b = *bytes.get(i)?;
                if b == 0xFF {
                    let n = *bytes.get(i + 1)?;
                    if n == 0x00 || (0xD0..=0xD7).contains(&n) || n == 0xFF {
                        i += if n == 0xFF { 1 } else { 2 };
                        continue;
                    }
                    break; // real marker: EOI or next scan (progressive)
                }
                i += 1;
            }
        }
    }
}

/// Is this a JPEG a standard decoder can display (baseline / extended /
/// progressive Huffman)? Excludes lossless JPEG (SOF3) used for DNG/CR2 raw data.
pub fn jpeg_is_displayable(bytes: &[u8]) -> bool {
    let mut i = 2;
    while i + 4 <= bytes.len() && bytes[i] == 0xFF {
        let m = bytes[i + 1];
        if matches!(m, 0xC0..=0xC2) {
            return true;
        }
        if matches!(m, 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) || m == 0xDA || m == 0xD9 {
            return false;
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        i += 2 + len;
    }
    false
}

/// Naive end search used when the marker walk fails (corrupt or truncated
/// headers): first `FF D9` after the SOI.
fn naive_extent(bytes: &[u8], start: usize) -> Option<usize> {
    (start + 3..bytes.len().saturating_sub(1))
        .find(|&e| bytes[e] == 0xFF && bytes[e + 1] == 0xD9)
        .map(|e| e + 2)
}

pub struct ZeroDecodePreviewExtractor;

impl ZeroDecodePreviewExtractor {
    /// Memory-maps a RAW file (`.CR3`, `.ARW`, `.NEF`, ...) and returns the
    /// largest embedded JPEG without demosaicing sensor data.
    pub fn find_largest_embedded_jpeg(raw_path: &Path) -> Result<Option<Vec<u8>>> {
        let file = File::open(raw_path)?;
        if file.metadata()?.len() == 0 {
            return Ok(None);
        }
        // SAFETY: read-only mapping dropped before returning; the caller must
        // not truncate the file concurrently.
        let mmap = unsafe { memmap2::MmapOptions::new().map(&file)? };
        let bytes: &[u8] = &mmap;

        // 1. Structured offsets.
        let meta = metadata::parse_bytes(bytes);
        let structured = meta
            .preview_ranges
            .iter()
            .filter_map(|&(s, e)| {
                let end = jpeg_extent(bytes, s).unwrap_or(e);
                let slice = bytes.get(s..end)?;
                jpeg_is_displayable(slice).then_some(slice)
            })
            .max_by_key(|s| s.len());
        if let Some(s) = structured {
            return Ok(Some(s.to_vec()));
        }
        // 2. Scan.
        Ok(Self::find_largest_jpeg_in(bytes).map(<[u8]>::to_vec))
    }

    /// Scans a byte slice for the largest complete JPEG.
    pub fn find_largest_jpeg_in(bytes: &[u8]) -> Option<&[u8]> {
        let mut best: Option<&[u8]> = None;
        let mut i = 0;
        while i + 3 < bytes.len() {
            if bytes[i] == 0xFF && bytes[i + 1] == 0xD8 && bytes[i + 2] == 0xFF {
                let end = jpeg_extent(bytes, i).or_else(|| naive_extent(bytes, i));
                match end {
                    Some(end) => {
                        let candidate = &bytes[i..end];
                        if best.is_none_or(|b| candidate.len() > b.len()) {
                            best = Some(candidate);
                        }
                        i = end;
                        continue;
                    }
                    // No end marker anywhere after this point.
                    None => break,
                }
            }
            i += 1;
        }
        best
    }
}

/// Options for rendering a viewable preview.
#[derive(Debug, Clone)]
pub struct PreviewOptions {
    /// Longest edge in pixels; 0 keeps the original size.
    pub max_size: u32,
    pub quality: u8,
    pub apply_orientation: bool,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        PreviewOptions {
            max_size: 2048,
            quality: 88,
            apply_orientation: true,
        }
    }
}

pub struct PreviewRenderer;

impl PreviewRenderer {
    /// Decodes the best available image for `path`: the embedded JPEG for
    /// RAW/JPEG files, otherwise a full decode (PNG/TIFF).
    pub fn decode(path: &Path) -> Result<DynamicImage> {
        if let Some(jpeg) = ZeroDecodePreviewExtractor::find_largest_embedded_jpeg(path)? {
            if let Ok(img) = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg) {
                return Ok(img);
            }
        }
        let bytes = std::fs::read(path)?;
        image::load_from_memory(&bytes).map_err(Into::into)
    }

    /// Renders a JPEG preview with optional LUT, orientation and resize.
    pub fn render(path: &Path, lut: Option<&CubeLut>, opts: &PreviewOptions) -> Result<Vec<u8>> {
        let mut img = Self::decode(path)?;
        if opts.apply_orientation {
            let orientation = metadata::read_metadata(path)
                .ok()
                .and_then(|m| m.orientation);
            img = apply_orientation(img, orientation.unwrap_or(1));
        }
        Self::finish(img, lut, opts)
    }

    /// Resizes, grades and encodes an already-decoded image.
    pub fn finish(
        img: DynamicImage,
        lut: Option<&CubeLut>,
        opts: &PreviewOptions,
    ) -> Result<Vec<u8>> {
        let img =
            if opts.max_size > 0 && (img.width() > opts.max_size || img.height() > opts.max_size) {
                img.resize(
                    opts.max_size,
                    opts.max_size,
                    image::imageops::FilterType::Triangle,
                )
            } else {
                img
            };
        let mut rgb: RgbImage = img.to_rgb8();
        if let Some(lut) = lut {
            lut.apply_to_image(&mut rgb);
        }
        encode_jpeg(&rgb, opts.quality)
    }
}

pub fn encode_jpeg(rgb: &RgbImage, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    enc.encode_image(rgb).map_err(AethrCraftError::from)?;
    Ok(out)
}

/// Applies an EXIF orientation value (1-8).
pub fn apply_orientation(img: DynamicImage, orientation: u16) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// Exports a single video frame via `ffmpeg` (must be on PATH), optionally
/// graded with a LUT. `seconds` is the offset from the clip start.
pub fn export_video_frame(
    video: &Path,
    seconds: f64,
    output: &Path,
    lut: Option<&CubeLut>,
    opts: &PreviewOptions,
) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!(
        "aethrcraft_frame_{}_{}.png",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let status = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss"])
        .arg(format!("{seconds:.3}"))
        .arg("-i")
        .arg(video)
        .args(["-frames:v", "1"])
        .arg(&tmp)
        .status()
        .map_err(|e| AethrCraftError::External(format!("ffmpeg not available: {e}")))?;
    if !status.success() {
        return Err(AethrCraftError::External(format!(
            "ffmpeg exited with {status}"
        )));
    }
    let img = image::open(&tmp);
    let _ = std::fs::remove_file(&tmp);
    let jpeg = PreviewRenderer::finish(img?, lut, opts)?;
    std::fs::write(output, jpeg)?;
    Ok(())
}
