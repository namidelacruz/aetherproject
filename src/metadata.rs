//! Zero-decode technical metadata extraction.
//!
//! Supports TIFF-based RAW (NEF, ARW, DNG, CR2, ORF, RW2, PEF, SRW...), JPEG
//! (APP1 Exif), Fujifilm RAF, ISOBMFF (CR3, HEIC, MP4, MOV) and Sony
//! NonRealTimeMeta XML sidecars (`C0001M01.XML`). Every read is bounds-checked;
//! malformed files yield partial metadata rather than errors.

use crate::error::Result;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::Serialize;
use std::collections::HashSet;
use std::fs::File;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct TechnicalMetadata {
    pub make: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    pub exposure_time: Option<String>,
    pub f_number: Option<f64>,
    pub focal_length: Option<f64>,
    pub capture_time: Option<NaiveDateTime>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Option<u16>,
    pub duration_seconds: Option<f64>,
    pub frame_rate: Option<f64>,
    pub start_timecode: Option<String>,
    /// Absolute byte ranges of embedded JPEG previews found via IFD/box offsets.
    #[serde(skip)]
    pub preview_ranges: Vec<(usize, usize)>,
}

impl TechnicalMetadata {
    /// Fills empty fields of `self` from `other`.
    pub fn fill_from(&mut self, other: TechnicalMetadata) {
        macro_rules! fill { ($($f:ident),*) => { $( if self.$f.is_none() { self.$f = other.$f; } )* } }
        fill!(
            make,
            model,
            serial,
            lens,
            iso,
            exposure_time,
            f_number,
            focal_length,
            capture_time,
            width,
            height,
            orientation,
            duration_seconds,
            frame_rate,
            start_timecode
        );
        self.preview_ranges.extend(other.preview_ranges);
    }

    pub fn capture_time_utc(&self) -> Option<DateTime<Utc>> {
        self.capture_time.map(|t| t.and_utc())
    }

    pub fn shutter_string(&self) -> String {
        self.exposure_time.clone().unwrap_or_default()
    }

    pub fn aperture_string(&self) -> String {
        self.f_number
            .map(|f| format!("f{}", trim_float(f)))
            .unwrap_or_default()
    }

    pub fn focal_string(&self) -> String {
        self.focal_length
            .map(|f| format!("{}mm", trim_float(f)))
            .unwrap_or_default()
    }
}

fn trim_float(f: f64) -> String {
    let s = format!("{f:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// Reads metadata from a file on disk (memory-mapped).
pub fn read_metadata(path: &Path) -> Result<TechnicalMetadata> {
    let file = File::open(path)?;
    if file.metadata()?.len() == 0 {
        return Ok(TechnicalMetadata::default());
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "xml" {
        let text = std::fs::read_to_string(path)?;
        return Ok(parse_sony_nrt_xml(&text));
    }
    // SAFETY: read-only mapping, dropped before return.
    let mmap = unsafe { memmap2::MmapOptions::new().map(&file)? };
    Ok(parse_bytes(&mmap))
}

/// Dispatches on the container's magic bytes.
pub fn parse_bytes(data: &[u8]) -> TechnicalMetadata {
    let mut meta = TechnicalMetadata::default();
    if data.len() < 12 {
        return meta;
    }
    if data.starts_with(b"FUJIFILMCCD-RAW") {
        parse_raf(data, &mut meta);
    } else if data[0] == 0xFF && data[1] == 0xD8 {
        parse_jpeg_exif(data, 0, &mut meta);
    } else if (data.starts_with(b"II") || data.starts_with(b"MM")) && data.len() > 8 {
        Tiff::new(data, 0).walk(&mut meta);
    } else if matches!(
        &data[4..8],
        b"ftyp" | b"moov" | b"wide" | b"mdat" | b"free" | b"skip"
    ) {
        parse_isobmff(data, &mut meta);
    }
    meta
}

// ---------------------------------------------------------------------------
// TIFF / EXIF
// ---------------------------------------------------------------------------

const TAG_COMPRESSION: u16 = 0x0103;
const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_STRIP_OFFSETS: u16 = 0x0111;
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_STRIP_BYTE_COUNTS: u16 = 0x0117;
const TAG_DATETIME: u16 = 0x0132;
const TAG_SUB_IFDS: u16 = 0x014A;
const TAG_JPEG_OFFSET: u16 = 0x0201;
const TAG_JPEG_LENGTH: u16 = 0x0202;
const TAG_EXPOSURE_TIME: u16 = 0x829A;
const TAG_F_NUMBER: u16 = 0x829D;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_ISO: u16 = 0x8827;
const TAG_DATETIME_ORIGINAL: u16 = 0x9003;
const TAG_FOCAL_LENGTH: u16 = 0x920A;
const TAG_PIXEL_X: u16 = 0xA002;
const TAG_PIXEL_Y: u16 = 0xA003;
const TAG_BODY_SERIAL: u16 = 0xA431;
const TAG_LENS_MODEL: u16 = 0xA434;
const TAG_DNG_CAMERA_SERIAL: u16 = 0xC62F;

#[derive(Debug, Clone, Copy)]
struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    /// Offset of the value bytes, relative to the TIFF header.
    value_off: usize,
}

struct Tiff<'a> {
    data: &'a [u8],
    /// Absolute offset of the TIFF header within the outer buffer (used to
    /// report preview ranges in file coordinates).
    base: usize,
    le: bool,
}

fn type_size(typ: u16) -> usize {
    match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => 0,
    }
}

impl<'a> Tiff<'a> {
    /// `data` must start at the TIFF header.
    fn new(data: &'a [u8], base: usize) -> Self {
        let le = data.starts_with(b"II");
        Tiff { data, base, le }
    }

    fn u16(&self, off: usize) -> Option<u16> {
        let b = self.data.get(off..off + 2)?;
        Some(if self.le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }

    fn u32(&self, off: usize) -> Option<u32> {
        let b = self.data.get(off..off + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if self.le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    }

    fn read_ifd(&self, off: usize) -> Option<(Vec<Entry>, usize)> {
        let n = self.u16(off)? as usize;
        if n == 0 || n > 1000 {
            return None;
        }
        let mut entries = Vec::with_capacity(n);
        for i in 0..n {
            let e = off + 2 + i * 12;
            let tag = self.u16(e)?;
            let typ = self.u16(e + 2)?;
            let count = self.u32(e + 4)?;
            let size = type_size(typ).saturating_mul(count as usize);
            let value_off = if size <= 4 {
                e + 8
            } else {
                self.u32(e + 8)? as usize
            };
            if size > 0 && value_off.saturating_add(size) <= self.data.len() {
                entries.push(Entry {
                    tag,
                    typ,
                    count,
                    value_off,
                });
            }
        }
        let next = self.u32(off + 2 + n * 12).unwrap_or(0) as usize;
        Some((entries, next))
    }

    fn ascii(&self, e: &Entry) -> Option<String> {
        let b = self.data.get(e.value_off..e.value_off + e.count as usize)?;
        let s: String = String::from_utf8_lossy(b)
            .trim_end_matches('\0')
            .trim()
            .to_string();
        (!s.is_empty()).then_some(s)
    }

    fn uint(&self, e: &Entry, idx: usize) -> Option<u32> {
        match e.typ {
            1 | 7 => self.data.get(e.value_off + idx).map(|&b| b as u32),
            3 | 8 => self.u16(e.value_off + idx * 2).map(u32::from),
            4 | 9 | 13 => self.u32(e.value_off + idx * 4),
            _ => None,
        }
    }

    fn rational(&self, e: &Entry) -> Option<(u32, u32)> {
        if e.typ != 5 && e.typ != 10 {
            return self.uint(e, 0).map(|v| (v, 1));
        }
        Some((self.u32(e.value_off)?, self.u32(e.value_off + 4)?))
    }

    fn walk(&self, meta: &mut TechnicalMetadata) {
        if self.data.len() < 8 {
            return;
        }
        let Some(first) = self.u32(4) else { return };
        let mut visited = HashSet::new();
        let mut next = first as usize;
        let mut depth = 0;
        // IFD0 -> IFD1 -> ... chain.
        while next != 0 && depth < 8 {
            match self.process_ifd(next, meta, &mut visited, 0) {
                Some(n) => next = n,
                None => break,
            }
            depth += 1;
        }
    }

    /// Processes one IFD (and its sub-IFDs); returns the next-IFD offset.
    fn process_ifd(
        &self,
        off: usize,
        meta: &mut TechnicalMetadata,
        visited: &mut HashSet<usize>,
        level: u8,
    ) -> Option<usize> {
        if level > 4 || !visited.insert(off) {
            return None;
        }
        let (entries, next) = self.read_ifd(off)?;
        let find = |tag: u16| entries.iter().find(|e| e.tag == tag);

        for e in &entries {
            match e.tag {
                TAG_MAKE => set_if_none(&mut meta.make, self.ascii(e)),
                TAG_MODEL => set_if_none(&mut meta.model, self.ascii(e)),
                TAG_ORIENTATION if level == 0 => {
                    if meta.orientation.is_none() {
                        meta.orientation = self.uint(e, 0).map(|v| v as u16)
                    }
                }
                TAG_DATETIME => {
                    if meta.capture_time.is_none() {
                        meta.capture_time = self.ascii(e).and_then(|s| parse_exif_datetime(&s));
                    }
                }
                TAG_DATETIME_ORIGINAL => {
                    // DateTimeOriginal beats IFD0 DateTime (modification time).
                    if let Some(t) = self.ascii(e).and_then(|s| parse_exif_datetime(&s)) {
                        meta.capture_time = Some(t);
                    }
                }
                TAG_EXPOSURE_TIME => {
                    if let Some((n, d)) = self.rational(e) {
                        if n > 0 && d > 0 {
                            meta.exposure_time = Some(if n < d {
                                format!("1/{}", (d as f64 / n as f64).round() as u64)
                            } else {
                                format!("{}s", trim_float(n as f64 / d as f64))
                            });
                        }
                    }
                }
                TAG_F_NUMBER => {
                    if let Some((n, d)) = self.rational(e).filter(|(_, d)| *d > 0) {
                        meta.f_number = Some(n as f64 / d as f64);
                    }
                }
                TAG_FOCAL_LENGTH => {
                    if let Some((n, d)) = self.rational(e).filter(|(_, d)| *d > 0) {
                        meta.focal_length = Some(n as f64 / d as f64);
                    }
                }
                TAG_ISO => set_if_none(&mut meta.iso, self.uint(e, 0).filter(|v| *v > 0)),
                TAG_PIXEL_X => meta.width = self.uint(e, 0).or(meta.width),
                TAG_PIXEL_Y => meta.height = self.uint(e, 0).or(meta.height),
                TAG_BODY_SERIAL | TAG_DNG_CAMERA_SERIAL => {
                    set_if_none(&mut meta.serial, self.ascii(e))
                }
                TAG_LENS_MODEL => set_if_none(&mut meta.lens, self.ascii(e)),
                TAG_EXIF_IFD => {
                    if let Some(o) = self.uint(e, 0) {
                        self.process_ifd(o as usize, meta, visited, level + 1);
                    }
                }
                TAG_SUB_IFDS => {
                    for i in 0..(e.count as usize).min(8) {
                        if let Some(o) = self.uint(e, i) {
                            self.process_ifd(o as usize, meta, visited, level + 1);
                        }
                    }
                }
                _ => {}
            }
        }

        // Embedded JPEG previews.
        if let (Some(o), Some(l)) = (find(TAG_JPEG_OFFSET), find(TAG_JPEG_LENGTH)) {
            if let (Some(o), Some(l)) = (self.uint(o, 0), self.uint(l, 0)) {
                self.push_preview(o as usize, l as usize, meta);
            }
        }
        let compression = find(TAG_COMPRESSION).and_then(|e| self.uint(e, 0));
        if matches!(compression, Some(6) | Some(7)) {
            if let (Some(so), Some(sc)) = (find(TAG_STRIP_OFFSETS), find(TAG_STRIP_BYTE_COUNTS)) {
                if so.count == 1 {
                    if let (Some(o), Some(l)) = (self.uint(so, 0), self.uint(sc, 0)) {
                        self.push_preview(o as usize, l as usize, meta);
                    }
                }
            }
        }
        Some(next)
    }

    fn push_preview(&self, off: usize, len: usize, meta: &mut TechnicalMetadata) {
        if len > 4
            && off.saturating_add(len) <= self.data.len()
            && self.data[off] == 0xFF
            && self.data[off + 1] == 0xD8
        {
            meta.preview_ranges
                .push((self.base + off, self.base + off + len));
        }
    }
}

fn set_if_none<T>(slot: &mut Option<T>, v: Option<T>) {
    if slot.is_none() {
        *slot = v;
    }
}

/// Parses `YYYY:MM:DD HH:MM:SS` (EXIF) and ISO-8601-ish variants.
pub fn parse_exif_datetime(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim().trim_end_matches('\0');
    for fmt in [
        "%Y:%m:%d %H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(t) = NaiveDateTime::parse_from_str(s.get(..19).unwrap_or(s), fmt) {
            return Some(t);
        }
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.naive_local());
    }
    None
}

/// Finds the APP1 Exif segment of a JPEG starting at `start` and parses it.
fn parse_jpeg_exif(data: &[u8], start: usize, meta: &mut TechnicalMetadata) {
    let mut i = start + 2;
    while i + 4 <= data.len() && data[i] == 0xFF {
        let marker = data[i + 1];
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if len < 2 {
            break;
        }
        let seg = i + 4;
        if marker == 0xE1 && data.get(seg..seg + 6) == Some(b"Exif\0\0") {
            let tiff_start = seg + 6;
            let end = (i + 2 + len).min(data.len());
            if tiff_start < end {
                Tiff::new(&data[tiff_start..end], tiff_start).walk(meta);
            }
        }
        if marker == 0xC0 || marker == 0xC1 || marker == 0xC2 {
            if let Some(b) = data.get(seg..seg + 5) {
                set_if_none(
                    &mut meta.height,
                    Some(u16::from_be_bytes([b[1], b[2]]) as u32),
                );
                set_if_none(
                    &mut meta.width,
                    Some(u16::from_be_bytes([b[3], b[4]]) as u32),
                );
            }
        }
        i += 2 + len;
    }
}

/// Fujifilm RAF: header holds the offset/length of an embedded JPEG
/// (which carries the EXIF block).
fn parse_raf(data: &[u8], meta: &mut TechnicalMetadata) {
    let be = |o: usize| {
        data.get(o..o + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    if let (Some(off), Some(len)) = (be(84), be(88)) {
        if off.saturating_add(len) <= data.len()
            && len > 4
            && data[off] == 0xFF
            && data[off + 1] == 0xD8
        {
            meta.preview_ranges.push((off, off + len));
            parse_jpeg_exif(&data[..off + len], off, meta);
        }
    }
}

// ---------------------------------------------------------------------------
// ISOBMFF (CR3 / HEIC / MP4 / MOV)
// ---------------------------------------------------------------------------

const CANON_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48,
];
const CANON_PRVW_UUID: [u8; 16] = [
    0xea, 0xf4, 0x2b, 0x5e, 0x1c, 0x98, 0x4b, 0x88, 0xb9, 0xfb, 0xb7, 0xdc, 0x40, 0x6e, 0x4d, 0x16,
];

struct BoxHeader {
    kind: [u8; 4],
    uuid: Option<[u8; 16]>,
    content: usize,
    end: usize,
}

fn read_box(data: &[u8], off: usize, limit: usize) -> Option<BoxHeader> {
    let h = data.get(off..off + 8)?;
    let size32 = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
    let kind = [h[4], h[5], h[6], h[7]];
    let (size, mut content) = match size32 {
        0 => (limit - off, off + 8),
        1 => {
            let b = data.get(off + 8..off + 16)?;
            (u64::from_be_bytes(b.try_into().ok()?) as usize, off + 16)
        }
        n => (n, off + 8),
    };
    if size < 8 {
        return None;
    }
    let end = off.checked_add(size)?;
    if end > limit {
        return None;
    }
    let uuid = if &kind == b"uuid" {
        let u: [u8; 16] = data.get(content..content + 16)?.try_into().ok()?;
        content += 16;
        Some(u)
    } else {
        None
    };
    Some(BoxHeader {
        kind,
        uuid,
        content,
        end,
    })
}

fn parse_isobmff(data: &[u8], meta: &mut TechnicalMetadata) {
    walk_boxes(data, 0, data.len(), 0, meta);
}

fn walk_boxes(data: &[u8], start: usize, end: usize, depth: u8, meta: &mut TechnicalMetadata) {
    if depth > 8 {
        return;
    }
    let mut off = start;
    while off + 8 <= end {
        let Some(b) = read_box(data, off, end) else {
            break;
        };
        match &b.kind {
            b"moov" | b"trak" | b"mdia" | b"minf" | b"udta" => {
                walk_boxes(data, b.content, b.end, depth + 1, meta)
            }
            b"mvhd" => parse_mvhd(&data[b.content..b.end], meta),
            b"tkhd" => parse_tkhd(&data[b.content..b.end], meta),
            b"uuid" if b.uuid == Some(CANON_UUID) => {
                walk_boxes(data, b.content, b.end, depth + 1, meta)
            }
            b"uuid" if b.uuid == Some(CANON_PRVW_UUID) => {
                // PRVW box: locate its JPEG without depending on field layout.
                if let Some(rel) = data[b.content..b.end]
                    .windows(3)
                    .position(|w| w == [0xFF, 0xD8, 0xFF])
                {
                    let s = b.content + rel;
                    if let Some(e) = crate::preview::jpeg_extent(&data[..b.end], s) {
                        meta.preview_ranges.push((s, e));
                    }
                }
            }
            b"CMT1" | b"CMT2" => {
                // CMT1 = IFD0, CMT2 = Exif IFD, each a standalone TIFF.
                let tiff = &data[b.content..b.end];
                if tiff.len() > 8 {
                    Tiff::new(tiff, b.content).walk(meta);
                }
            }
            _ => {}
        }
        off = b.end;
    }
}

fn mp4_epoch_to_naive(secs: u64) -> Option<NaiveDateTime> {
    if secs == 0 {
        return None;
    }
    let base = NaiveDate::from_ymd_opt(1904, 1, 1)?.and_hms_opt(0, 0, 0)?;
    base.checked_add_signed(chrono::Duration::seconds(secs as i64))
}

fn parse_mvhd(c: &[u8], meta: &mut TechnicalMetadata) {
    let be32 = |o: usize| {
        c.get(o..o + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64)
    };
    let be64 = |o: usize| {
        c.get(o..o + 8)
            .map(|b| u64::from_be_bytes(b.try_into().unwrap()))
    };
    let (created, timescale, duration) = match c.first() {
        Some(1) => (be64(4), be32(20), be64(24)),
        Some(0) => (be32(4), be32(12), be32(16)),
        _ => return,
    };
    if meta.capture_time.is_none() {
        meta.capture_time = created.and_then(mp4_epoch_to_naive);
    }
    if let (Some(ts), Some(d)) = (timescale, duration) {
        if ts > 0 && d > 0 && d != u32::MAX as u64 {
            meta.duration_seconds = Some(d as f64 / ts as f64);
        }
    }
}

fn parse_tkhd(c: &[u8], meta: &mut TechnicalMetadata) {
    let off = match c.first() {
        Some(1) => 88,
        Some(0) => 76,
        _ => return,
    };
    let fixed = |o: usize| {
        c.get(o..o + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) >> 16)
    };
    if let (Some(w), Some(h)) = (fixed(off), fixed(off + 4)) {
        if w > 0 && h > 0 && w >= meta.width.unwrap_or(0) {
            meta.width = Some(w);
            meta.height = Some(h);
        }
    }
}

// ---------------------------------------------------------------------------
// Sony NonRealTimeMeta XML (XAVC / XDCAM clip sidecars)
// ---------------------------------------------------------------------------

/// Parses the fields AethrCraft uses from a Sony `M01.XML` sidecar.
pub fn parse_sony_nrt_xml(text: &str) -> TechnicalMetadata {
    use quick_xml::events::Event;
    let mut meta = TechnicalMetadata::default();
    let mut reader = quick_xml::Reader::from_str(text);
    let mut ltc_fps: Option<f64> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.local_name();
                let attr = |key: &[u8]| {
                    e.attributes()
                        .flatten()
                        .find(|a| a.key.local_name().as_ref() == key)
                        .and_then(|a| a.unescape_value().ok().map(|v| v.to_string()))
                };
                match name.as_ref() {
                    b"Device" => {
                        set_if_none(&mut meta.make, attr(b"manufacturer"));
                        set_if_none(&mut meta.model, attr(b"modelName"));
                        set_if_none(&mut meta.serial, attr(b"serialNo"));
                    }
                    b"Lens" => set_if_none(&mut meta.lens, attr(b"modelName")),
                    b"CreationDate" => {
                        if let Some(v) = attr(b"value") {
                            meta.capture_time = parse_exif_datetime(&v);
                        }
                    }
                    b"VideoFrame" => {
                        if let Some(v) = attr(b"captureFps") {
                            meta.frame_rate = parse_fps_label(&v);
                        }
                    }
                    b"LtcChangeTable" => {
                        ltc_fps = attr(b"tcFps").and_then(|v| v.parse().ok());
                    }
                    b"LtcChange" => {
                        if meta.start_timecode.is_none()
                            && attr(b"frameCount").as_deref() == Some("0")
                        {
                            if let Some(v) = attr(b"value") {
                                meta.start_timecode = decode_sony_ltc(&v);
                            }
                        }
                    }
                    b"Duration" => {
                        if let Some(frames) = attr(b"value").and_then(|v| v.parse::<f64>().ok()) {
                            meta.duration_seconds = Some(frames); // frames for now
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    // Duration is stored in frames; convert using the best known rate.
    if let Some(frames) = meta.duration_seconds {
        match meta.frame_rate.or(ltc_fps) {
            Some(fps) if fps > 0.0 => meta.duration_seconds = Some(frames / fps),
            _ => meta.duration_seconds = None,
        }
    }
    meta
}

fn parse_fps_label(v: &str) -> Option<f64> {
    let num: String = v
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let f: f64 = num.parse().ok()?;
    // Sony labels NTSC rates as 23.98p / 29.97p / 59.94p already; integer
    // labels such as "25p" are exact.
    Some(f)
}

/// Sony stores LTC as BCD `FFSSMMHH` hex digits.
fn decode_sony_ltc(v: &str) -> Option<String> {
    if v.len() != 8 || !v.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let digits = |s: &str| -> Option<u32> {
        let n = u32::from_str_radix(s, 16).ok()? & 0x3F; // strip drop-frame / colour bits
        Some((n >> 4) * 10 + (n & 0xF))
    };
    let ff = digits(&v[0..2])?;
    let ss = digits(&v[2..4])?;
    let mm = digits(&v[4..6])?;
    let hh = digits(&v[6..8])?;
    Some(format!("{hh:02}:{mm:02}:{ss:02}:{ff:02}"))
}
