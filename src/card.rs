//! Camera-card structure detection and the virtual asset graph.
//!
//! A scan walks a source root once, detects every camera structure on it and
//! assigns **each file to exactly one asset**. When structures overlap (a Sony
//! card carries both `DCIM` and `PRIVATE/M4ROOT`; a RED `.RDC` sits inside an
//! `.RDM`), the deepest structure root wins. Inside a structure, files that are
//! not part of a clip (indexes, `MEDIAPRO.XML`, `STATUS.BIN`...) become a
//! *support* asset, so that preserving the structure never drops a file.

use crate::error::Result;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

/// Directory used for sidecars of assets inside locked camera structures.
pub const SIDECAR_DIR: &str = "_aethrcraft_sidecars";
/// ASC-MHL history directory.
pub const MHL_DIR: &str = "ascmhl";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, PartialOrd, Ord)]
pub enum CardFormat {
    /// Sony XAVC / XAVC S (`PRIVATE/M4ROOT`)
    SonyXavc,
    /// Sony XDCAM EX (`BPAV`)
    SonyXdcamEx,
    /// Sony XDCAM / SxS (`XDROOT`)
    SonyXdcam,
    /// Canon XF (`CONTENTS/CLIPS001`)
    CanonXf,
    /// Panasonic P2 (`CONTENTS/CLIP` + `VIDEO`)
    PanasonicP2,
    /// AVCHD (`BDMV`, `PRIVATE/AVCHD/BDMV`)
    Avchd,
    /// RED (`*.RDC` clip folders)
    Red,
    /// Frame sequences (ARRIRAW `.ari`, CinemaDNG)
    FrameSequence,
    /// DCF stills/video (`DCIM`)
    Dcim,
    /// Anything else.
    Generic,
}

impl CardFormat {
    /// Structures whose internal layout must not be altered.
    pub fn is_locked(self) -> bool {
        !matches!(self, CardFormat::Dcim | CardFormat::Generic)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, PartialOrd, Ord)]
pub enum AssetKind {
    Video,
    Still,
    Audio,
    /// Non-media files outside camera structures (documents, notes...).
    Other,
    /// Non-clip files inside a camera structure.
    Support,
}

const VIDEO_EXT: &[&str] = &[
    "mp4", "mov", "mxf", "mts", "m2ts", "avi", "r3d", "braw", "ari", "mkv", "crm", "mpg", "mpeg",
    "m4v", "insv", "360", "dng_seq",
];
const RAW_EXT: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef",
    "srw", "x3f", "3fr", "iiq", "erf", "mos", "gpr",
];
const STILL_EXT: &[&str] = &[
    "jpg", "jpeg", "heic", "heif", "hif", "png", "tif", "tiff", "webp",
];
const AUDIO_EXT: &[&str] = &["wav", "mp3", "aac", "m4a", "flac", "aif", "aiff", "bwf"];

pub fn ext_of(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn kind_for_ext(ext: &str) -> Option<AssetKind> {
    if VIDEO_EXT.contains(&ext) {
        Some(AssetKind::Video)
    } else if RAW_EXT.contains(&ext) || STILL_EXT.contains(&ext) {
        Some(AssetKind::Still)
    } else if AUDIO_EXT.contains(&ext) {
        Some(AssetKind::Audio)
    } else {
        None
    }
}

/// Ranking used to choose an asset's primary file (lower wins).
fn primary_rank(ext: &str) -> u8 {
    if VIDEO_EXT.contains(&ext) {
        0
    } else if RAW_EXT.contains(&ext) {
        1
    } else if STILL_EXT.contains(&ext) {
        2
    } else if AUDIO_EXT.contains(&ext) {
        3
    } else {
        9
    }
}

fn is_ignored_name(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "Thumbs.db"
                | "desktop.ini"
                | "System Volume Information"
                | "$RECYCLE.BIN"
                | SIDECAR_DIR
                | MHL_DIR
        )
}

#[derive(Debug, Clone, Serialize)]
pub struct DetectedStructure {
    pub format: CardFormat,
    /// Relative to the scan root.
    pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct Asset {
    /// Stable identifier: the primary file's relative path with `/`.
    pub id: String,
    pub kind: AssetKind,
    pub format: CardFormat,
    pub structure_root: Option<PathBuf>,
    /// Members may not be renamed or separated.
    pub locked: bool,
    /// Relative to the scan root.
    pub primary: PathBuf,
    /// Relative paths, primary first.
    pub members: Vec<PathBuf>,
    /// For folder-unit assets (`.RDC`, frame sequences): the folder copied as
    /// a whole, relative to the scan root.
    pub group_dir: Option<PathBuf>,
    pub total_size: u64,
    #[serde(skip)]
    pub modified: Option<SystemTime>,
}

impl Asset {
    pub fn is_media(&self) -> bool {
        matches!(
            self.kind,
            AssetKind::Video | AssetKind::Still | AssetKind::Audio
        )
    }

    pub fn display_name(&self) -> String {
        match &self.group_dir {
            Some(d) => d
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            None => self
                .primary
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        }
    }

    /// Members with an `.xmp` extension (camera or third-party sidecars).
    pub fn xmp_members(&self) -> impl Iterator<Item = &PathBuf> {
        self.members.iter().filter(|m| ext_of(m) == "xmp")
    }

    /// Where AethrCraft stores this asset's XMP sidecar, relative to the scan
    /// root:
    /// * folder units (`.RDC`, sequences): `<dir>.xmp` next to the folder;
    /// * clips inside other locked structures: `_aethrcraft_sidecars/<primary>.xmp`
    ///   so camera folders are never polluted;
    /// * everything else: `<primary stem>.xmp` beside the primary file.
    pub fn sidecar_rel_path(&self) -> PathBuf {
        if let Some(dir) = &self.group_dir {
            let mut name = dir.file_name().unwrap_or_default().to_os_string();
            name.push(".xmp");
            return dir.with_file_name(name);
        }
        if self.locked {
            let mut p = PathBuf::from(SIDECAR_DIR).join(&self.primary);
            let mut name = p.file_name().unwrap_or_default().to_os_string();
            name.push(".xmp");
            p.set_file_name(name);
            return p;
        }
        if let Some(existing) = self.xmp_members().next() {
            return existing.clone();
        }
        self.primary.with_extension("xmp")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CardScan {
    pub root: PathBuf,
    pub card_name: String,
    pub structures: Vec<DetectedStructure>,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone)]
struct FileInfo {
    rel: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

fn lower_components(p: &Path) -> Vec<String> {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect()
}

/// Strips trailing media/sidecar extensions so `IMG_1.CR3.xmp` and `IMG_1.JPG`
/// share the group key `img_1`.
fn base_stem(name: &str) -> String {
    let mut s = name.to_ascii_lowercase();
    loop {
        match s.rsplit_once('.') {
            Some((head, ext))
                if !head.is_empty()
                    && (kind_for_ext(ext).is_some()
                        || matches!(
                            ext,
                            "xmp" | "thm" | "lrv" | "xml" | "wav" | "rdc" | "lrf"
                        )) =>
            {
                s = head.to_string()
            }
            _ => return s,
        }
    }
}

impl CardScan {
    pub fn scan(root: &Path) -> Result<CardScan> {
        let root = root.to_path_buf();
        let card_name = root
            .canonicalize()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .or_else(|| root.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "CARD".to_string());

        let mut files = Vec::new();
        let mut dirs = Vec::new();
        let walker = WalkDir::new(&root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !is_ignored_name(&e.file_name().to_string_lossy()));
        for entry in walker {
            let entry = entry.map_err(|e| std::io::Error::other(e.to_string()))?;
            if entry.depth() == 0 {
                continue;
            }
            let rel = entry.path().strip_prefix(&root).unwrap().to_path_buf();
            if entry.file_type().is_dir() {
                dirs.push(rel);
            } else if entry.file_type().is_file() {
                let md = entry.metadata().ok();
                files.push(FileInfo {
                    rel,
                    size: md.as_ref().map(|m| m.len()).unwrap_or(0),
                    modified: md.and_then(|m| m.modified().ok()),
                });
            }
        }

        let structures = detect_structures(&dirs, &files);
        let assets = group_assets(&structures, files);
        Ok(CardScan {
            root,
            card_name,
            structures,
            assets,
        })
    }

    pub fn media_assets(&self) -> impl Iterator<Item = &Asset> {
        self.assets.iter().filter(|a| a.is_media())
    }

    pub fn find(&self, id: &str) -> Option<&Asset> {
        let norm = id.replace('\\', "/");
        self.assets
            .iter()
            .find(|a| a.id == norm || a.members.iter().any(|m| path_id(m) == norm))
    }
}

pub fn path_id(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn detect_structures(dirs: &[PathBuf], files: &[FileInfo]) -> Vec<DetectedStructure> {
    let mut out = Vec::new();
    let has_child = |dir: &Path, name: &str| {
        dirs.iter().any(|d| {
            d.parent() == Some(dir)
                && d.file_name()
                    .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
        })
    };
    for d in dirs {
        let comps = lower_components(d);
        let last = comps.last().map(String::as_str).unwrap_or("");
        let parent_last = comps.len().checked_sub(2).map(|i| comps[i].as_str());
        let format = match last {
            "m4root" if parent_last == Some("private") => Some(CardFormat::SonyXavc),
            "bpav" => Some(CardFormat::SonyXdcamEx),
            "xdroot" => Some(CardFormat::SonyXdcam),
            "contents" if has_child(d, "CLIP") && has_child(d, "VIDEO") => {
                Some(CardFormat::PanasonicP2)
            }
            "contents"
                if dirs.iter().any(|c| {
                    c.parent() == Some(d.as_path())
                        && c.file_name().is_some_and(|n| {
                            n.to_string_lossy()
                                .to_ascii_lowercase()
                                .starts_with("clips0")
                        })
                }) =>
            {
                Some(CardFormat::CanonXf)
            }
            "bdmv" => Some(CardFormat::Avchd),
            "dcim" => Some(CardFormat::Dcim),
            l if l.ends_with(".rdc") => Some(CardFormat::Red),
            _ => None,
        };
        if let Some(format) = format {
            // AVCHD: lock the enclosing AVCHD folder when present.
            let root = if format == CardFormat::Avchd && parent_last == Some("avchd") {
                d.parent().unwrap().to_path_buf()
            } else {
                d.clone()
            };
            if !out.iter().any(|s: &DetectedStructure| s.root == root) {
                out.push(DetectedStructure { format, root });
            }
            continue;
        }
        // Frame sequences: ≥2 .ari frames, or ≥24 .dng frames outside DCIM.
        let in_dcim = comps.iter().any(|c| c == "dcim");
        let count = |ext: &str| {
            files
                .iter()
                .filter(|f| f.rel.parent() == Some(d.as_path()) && ext_of(&f.rel) == ext)
                .count()
        };
        if count("ari") >= 2 || (!in_dcim && count("dng") >= 24) {
            out.push(DetectedStructure {
                format: CardFormat::FrameSequence,
                root: d.clone(),
            });
        }
    }
    // Deepest roots first so lookups pick the most specific structure.
    out.sort_by_key(|s| std::cmp::Reverse(s.root.components().count()));
    out
}

fn build_asset(
    format: CardFormat,
    structure_root: Option<&Path>,
    mut members: Vec<&FileInfo>,
    kind_override: Option<AssetKind>,
    group_dir: Option<PathBuf>,
) -> Asset {
    members.sort_by(|a, b| {
        primary_rank(&ext_of(&a.rel))
            .cmp(&primary_rank(&ext_of(&b.rel)))
            .then(a.rel.cmp(&b.rel))
    });
    let primary = members[0].rel.clone();
    let kind = kind_override
        .or_else(|| kind_for_ext(&ext_of(&primary)))
        .unwrap_or(if format.is_locked() {
            AssetKind::Support
        } else {
            AssetKind::Other
        });
    Asset {
        id: path_id(group_dir.as_deref().unwrap_or(&primary)),
        kind,
        format,
        structure_root: structure_root.map(Path::to_path_buf),
        locked: format.is_locked(),
        primary,
        total_size: members.iter().map(|m| m.size).sum(),
        modified: members.iter().filter_map(|m| m.modified).min(),
        members: members.iter().map(|m| m.rel.clone()).collect(),
        group_dir,
    }
}

/// Validates the suffix left after a primary-stem prefix match
/// (`M01`, `S03`, `T01`, `R01`, P2 audio `00`..`15`).
fn valid_clip_suffix(rest: &str) -> bool {
    if rest.is_empty() {
        return true;
    }
    let b = rest.as_bytes();
    (rest.len() == 2 && b.iter().all(u8::is_ascii_digit))
        || (rest.len() == 3 && b[0].is_ascii_alphabetic() && b[1..].iter().all(u8::is_ascii_digit))
}

fn group_assets(structures: &[DetectedStructure], files: Vec<FileInfo>) -> Vec<Asset> {
    let mut by_structure: BTreeMap<Option<usize>, Vec<&FileInfo>> = BTreeMap::new();
    for f in &files {
        let idx = structures.iter().position(|s| f.rel.starts_with(&s.root));
        by_structure.entry(idx).or_default().push(f);
    }

    let mut assets = Vec::new();
    for (idx, group) in by_structure {
        match idx {
            None => assets.extend(group_loose(CardFormat::Generic, None, &group)),
            Some(i) => {
                let s = &structures[i];
                assets.extend(group_structure(s, &group));
            }
        }
    }
    attach_folder_sidecars(&mut assets);
    assets.sort_by(|a, b| a.id.cmp(&b.id));
    assets
}

/// Generic / DCIM grouping: same directory + same base stem.
fn group_loose(format: CardFormat, root: Option<&Path>, files: &[&FileInfo]) -> Vec<Asset> {
    let mut groups: BTreeMap<(PathBuf, String), Vec<&FileInfo>> = BTreeMap::new();
    for f in files {
        let dir = f.rel.parent().unwrap_or(Path::new("")).to_path_buf();
        let name = f.rel.file_name().unwrap_or_default().to_string_lossy();
        groups.entry((dir, base_stem(&name))).or_default().push(f);
    }
    groups
        .into_values()
        .map(|members| build_asset(format, root, members, None, None))
        .collect()
}

fn group_structure(s: &DetectedStructure, files: &[&FileInfo]) -> Vec<Asset> {
    let root = s.root.as_path();
    let rel_in = |f: &FileInfo| f.rel.strip_prefix(root).unwrap().to_path_buf();
    match s.format {
        CardFormat::Dcim => group_loose(CardFormat::Dcim, Some(root), files),
        CardFormat::Red | CardFormat::FrameSequence => {
            vec![build_asset(
                s.format,
                Some(root),
                files.to_vec(),
                Some(AssetKind::Video),
                Some(root.to_path_buf()),
            )]
        }
        CardFormat::SonyXdcamEx | CardFormat::CanonXf => {
            // Clip = directory two levels below the root (BPAV/CLPR/<clip>,
            // CONTENTS/CLIPS001/<clip>).
            let mut clips: BTreeMap<PathBuf, Vec<&FileInfo>> = BTreeMap::new();
            let mut support = Vec::new();
            for f in files {
                let r = rel_in(f);
                let comps: Vec<_> = r.components().collect();
                let is_clip_area = comps.len() >= 3
                    && match s.format {
                        CardFormat::SonyXdcamEx => {
                            comps[0].as_os_str().eq_ignore_ascii_case("CLPR")
                        }
                        _ => comps[0]
                            .as_os_str()
                            .to_string_lossy()
                            .to_ascii_uppercase()
                            .starts_with("CLIPS"),
                    };
                if is_clip_area {
                    let key = root.join(comps[0].as_os_str()).join(comps[1].as_os_str());
                    clips.entry(key).or_default().push(f);
                } else {
                    support.push(*f);
                }
            }
            let mut out = Vec::new();
            for m in clips.into_values() {
                if m.iter().any(|f| kind_for_ext(&ext_of(&f.rel)).is_some()) {
                    out.push(build_asset(s.format, Some(root), m, None, None));
                } else {
                    // Clip folders without media count as support files.
                    support.extend(m);
                }
            }
            if !support.is_empty() {
                out.push(build_asset(
                    s.format,
                    Some(root),
                    support,
                    Some(AssetKind::Support),
                    None,
                ));
            }
            out
        }
        CardFormat::SonyXavc
        | CardFormat::SonyXdcam
        | CardFormat::PanasonicP2
        | CardFormat::Avchd => {
            let primary_dir: &[&str] = match s.format {
                CardFormat::SonyXavc | CardFormat::SonyXdcam => &["clip"],
                CardFormat::PanasonicP2 => &["video"],
                _ => &["stream"],
            };
            // Primary stems: media files in the primary directory.
            let mut stems: Vec<String> = files
                .iter()
                .filter(|f| {
                    let r = rel_in(f);
                    let comps = lower_components(r.parent().unwrap_or(Path::new("")));
                    comps
                        .last()
                        .is_some_and(|c| primary_dir.contains(&c.as_str()))
                        && kind_for_ext(&ext_of(&f.rel)) == Some(AssetKind::Video)
                })
                .filter_map(|f| {
                    f.rel
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_ascii_lowercase())
                })
                .collect();
            stems.sort_by_key(|s| std::cmp::Reverse(s.len()));
            stems.dedup();

            let mut clips: HashMap<String, Vec<&FileInfo>> = HashMap::new();
            let mut support = Vec::new();
            for f in files {
                let stem = f
                    .rel
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                // Longest primary stem that prefixes this file with a valid suffix.
                match stems
                    .iter()
                    .find(|p| stem.starts_with(p.as_str()) && valid_clip_suffix(&stem[p.len()..]))
                {
                    Some(p) => clips.entry(p.clone()).or_default().push(f),
                    None => support.push(*f),
                }
            }
            let mut out: Vec<Asset> = clips
                .into_values()
                .map(|m| build_asset(s.format, Some(root), m, Some(AssetKind::Video), None))
                .collect();
            if !support.is_empty() {
                out.push(build_asset(
                    s.format,
                    Some(root),
                    support,
                    Some(AssetKind::Support),
                    None,
                ));
            }
            out
        }
        CardFormat::Generic => group_loose(CardFormat::Generic, None, files),
    }
}

/// Moves `<dir>.xmp` files next to folder-unit assets into those assets.
fn attach_folder_sidecars(assets: &mut Vec<Asset>) {
    let targets: Vec<(usize, PathBuf)> = assets
        .iter()
        .enumerate()
        .filter_map(|(i, a)| a.group_dir.as_ref().map(|_| (i, a.sidecar_rel_path())))
        .collect();
    for (owner, sidecar) in targets {
        if let Some(pos) = assets
            .iter()
            .position(|a| a.members.len() == 1 && a.members[0] == sidecar)
        {
            let side = assets.remove(pos);
            let owner = if pos < owner { owner - 1 } else { owner };
            assets[owner].members.push(side.members[0].clone());
            assets[owner].total_size += side.total_size;
        }
    }
}
