//! Card offload: planning (pure, dry-run-able) and execution (single-pass
//! read, in-flight multi-hash, N destinations, verify, sidecars, ASC-MHL).
//!
//! ## Conflict rules
//! * **Rename vs. structure lock.** Assets in locked camera structures are
//!   never renamed. With `preserve_camera_card_structure` the whole card
//!   layout is copied under `<folder>/<card name>/`; without it, clip files
//!   keep their names and folder units (`.RDC`, frame sequences) stay intact,
//!   while structure support files (indexes) are skipped because they would be
//!   meaningless outside their structure.
//! * **Per-asset folders vs. structure integrity.** The folder pattern for a
//!   preserved card is evaluated once (earliest capture in that card), so a
//!   structure is never split across date folders.
//! * **Paired files.** RAW+JPEG(+WAV, +THM...) share one sequence number and
//!   one new stem; collision suffixes apply to the whole group.
//! * **Name collisions** are detected case-insensitively (exFAT/APFS/NTFS)
//!   both within the plan and against every destination.
//! * **XMP**: source XMP files are merged into the written sidecar instead of
//!   being copied verbatim (when sidecars are enabled), and are therefore
//!   excluded from ASC-MHL manifests.
//! * **Hash formats.** The verification algorithm and the MHL algorithm are
//!   computed in the same pass when they differ.

use crate::card::{ext_of, Asset, AssetKind, CardScan};
use crate::catalog::read_asset_metadata;
use crate::error::{AethrCraftError, Result};
use crate::hashing::{self, MultiHasher, CHUNK_SIZE};
use crate::metadata::TechnicalMetadata;
use crate::mhl::{self, HashAction, MhlEntry, MhlProcess};
use crate::profile::{
    ChecksumAlgorithm, CollisionPolicy, UnifiedIngestProfile, UnifiedMetadataTemplate,
};
use crate::variables::{AssetContext, VariableEngine};
use crate::xmp::XmpSidecar;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ---------------------------------------------------------------------------
// Single-file engine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct IngestReceipt {
    pub source_file: PathBuf,
    pub written_destinations: Vec<PathBuf>,
    pub bytes_copied: u64,
    pub checksum_algorithm: ChecksumAlgorithm,
    pub digest_hex: String,
}

pub struct SinglePassIngestEngine;

fn part_path(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    dest.with_file_name(format!(".{name}.aethrpart"))
}

impl SinglePassIngestEngine {
    /// Reads the source once, writes each chunk to all destinations, hashes
    /// in flight, then re-reads each destination to verify.
    pub async fn offload_asset(
        source: &Path,
        destinations: &[PathBuf],
        algorithm: ChecksumAlgorithm,
    ) -> Result<IngestReceipt> {
        let (bytes, digests) =
            Self::copy_verified(source, destinations, &[algorithm], algorithm).await?;
        Ok(IngestReceipt {
            source_file: source.to_path_buf(),
            written_destinations: destinations.to_vec(),
            bytes_copied: bytes,
            checksum_algorithm: algorithm,
            digest_hex: digests
                .get(&algorithm)
                .cloned()
                .unwrap_or_else(|| "UNVERIFIED".into()),
        })
    }

    /// Copies `source` to every path in `destinations` using temporary
    /// `.aethrpart` files that are renamed into place only after verification,
    /// so an interrupted offload never leaves a truncated file under its final
    /// name. Returns bytes copied and the digests for `algos`.
    pub async fn copy_verified(
        source: &Path,
        destinations: &[PathBuf],
        algos: &[ChecksumAlgorithm],
        verify: ChecksumAlgorithm,
    ) -> Result<(u64, HashMap<ChecksumAlgorithm, String>)> {
        let mut src = tokio::fs::File::open(source).await?;
        let src_mtime = src.metadata().await?.modified().ok();
        let mut outs = Vec::with_capacity(destinations.len());
        for d in destinations {
            if let Some(p) = d.parent() {
                tokio::fs::create_dir_all(p).await?;
            }
            let tmp = part_path(d);
            let f = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .await?;
            outs.push((tmp, f));
        }

        let mut all = algos.to_vec();
        if verify != ChecksumAlgorithm::None && !all.contains(&verify) {
            all.push(verify);
        }
        let mut hasher = MultiHasher::new(&all);
        let mut buf = vec![0u8; CHUNK_SIZE];
        let mut total = 0u64;
        let result: Result<()> = async {
            loop {
                let n = src.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                hasher.update(&buf[..n]);
                for (_, f) in outs.iter_mut() {
                    f.write_all(&buf[..n]).await?;
                }
            }
            for (_, f) in outs.iter_mut() {
                f.flush().await?;
                f.sync_all().await?;
            }
            Ok(())
        }
        .await;
        let tmps: Vec<PathBuf> = outs.into_iter().map(|(p, _)| p).collect();
        let cleanup = |tmps: &[PathBuf]| {
            for t in tmps {
                let _ = std::fs::remove_file(t);
            }
        };
        if let Err(e) = result {
            cleanup(&tmps);
            return Err(e);
        }
        let digests = hasher.finalize();

        if verify != ChecksumAlgorithm::None {
            let expected = digests.get(&verify).cloned().unwrap_or_default();
            for (tmp, dest) in tmps.iter().zip(destinations) {
                let actual = hashing::hash_file(tmp, &[verify])
                    .await?
                    .remove(&verify)
                    .unwrap_or_default();
                if actual != expected {
                    cleanup(&tmps);
                    return Err(AethrCraftError::ChecksumMismatch {
                        path: dest.clone(),
                        expected,
                        actual,
                    });
                }
            }
        }
        for (tmp, dest) in tmps.iter().zip(destinations) {
            if cfg!(windows) && dest.exists() {
                std::fs::remove_file(dest)?;
            }
            std::fs::rename(tmp, dest)?;
            if let Some(m) = src_mtime {
                if let Ok(f) = std::fs::OpenOptions::new().write(true).open(dest) {
                    let _ = f.set_modified(m);
                }
            }
        }
        Ok((total, digests))
    }
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CopyMode {
    /// Target does not exist yet.
    New,
    /// Target exists and will be replaced (Overwrite policy).
    Overwrite,
    /// Target exists and is kept; it is re-hashed and must match (Skip policy).
    KeepExisting,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedFile {
    /// Relative to the source root.
    pub source: PathBuf,
    /// Relative to each destination root.
    pub target: PathBuf,
    pub size: u64,
    pub mode: CopyMode,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedAsset {
    pub asset_id: String,
    pub kind: AssetKind,
    pub sequence: Option<u64>,
    pub files: Vec<PlannedFile>,
    /// Sidecar to write, relative to each destination root.
    pub sidecar: Option<PathBuf>,
    /// Existing XMP sources (relative to the source root), lowest priority first.
    pub source_sidecars: Vec<PathBuf>,
    #[serde(skip)]
    pub context: AssetContext,
}

#[derive(Debug, Clone, Serialize)]
pub struct IngestPlan {
    pub source_root: PathBuf,
    pub card_name: String,
    #[serde(skip)]
    pub profile: UnifiedIngestProfile,
    pub assets: Vec<PlannedAsset>,
    pub skipped_support_files: usize,
    pub warnings: Vec<String>,
    pub next_sequence: u64,
}

impl IngestPlan {
    pub fn total_bytes(&self) -> u64 {
        self.assets
            .iter()
            .flat_map(|a| &a.files)
            .map(|f| f.size)
            .sum()
    }

    pub fn file_count(&self) -> usize {
        self.assets.iter().map(|a| a.files.len()).sum()
    }
}

struct TargetIndex<'a> {
    destinations: &'a [PathBuf],
    planned: HashSet<String>,
}

impl<'a> TargetIndex<'a> {
    fn key(p: &Path) -> String {
        crate::card::path_id(p).to_lowercase()
    }
    fn planned(&self, p: &Path) -> bool {
        self.planned.contains(&Self::key(p))
    }
    fn exists(&self, p: &Path) -> bool {
        self.destinations.iter().any(|d| d.join(p).exists())
    }
    fn taken(&self, p: &Path) -> bool {
        self.planned(p) || self.exists(p)
    }
    fn claim(&mut self, p: &Path) {
        self.planned.insert(Self::key(p));
    }
}

fn file_name_string(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn stem_string(p: &Path) -> String {
    p.file_stem()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn join_rel(base: &str, rest: &Path) -> PathBuf {
    let mut p = PathBuf::new();
    for c in base.split('/').filter(|c| !c.is_empty()) {
        p.push(c);
    }
    p.push(rest);
    p
}

pub struct IngestPlanner;

impl IngestPlanner {
    pub fn plan(source_root: &Path, profile: &UnifiedIngestProfile) -> Result<IngestPlan> {
        let mut profile = profile.clone();
        let mut warnings = profile.validate()?;
        if profile.destinations.is_empty() {
            return Err(AethrCraftError::SchemaError(
                "profile has no destinations".into(),
            ));
        }
        let abs = |p: &Path| p.canonicalize().or_else(|_| std::path::absolute(p));
        let src_abs = abs(source_root)?;
        for d in &profile.destinations {
            let dst_abs = abs(d)?;
            if dst_abs.starts_with(&src_abs) || src_abs.starts_with(&dst_abs) {
                return Err(AethrCraftError::Conflict(format!(
                    "destination {} overlaps the source {}",
                    d.display(),
                    source_root.display()
                )));
            }
        }
        for t in VariableEngine::unknown_tokens(&profile.folder_naming_pattern)
            .into_iter()
            .chain(VariableEngine::unknown_tokens(&profile.file_naming_pattern))
        {
            warnings.push(format!("unknown token {t} will be kept literally"));
        }

        let scan = CardScan::scan(source_root)?;
        let ingest_ts = Utc::now();

        // Metadata + capture time for every asset.
        let mut infos: Vec<(Asset, TechnicalMetadata, DateTime<Utc>)> = scan
            .assets
            .iter()
            .map(|a| {
                let meta = if a.is_media() {
                    read_asset_metadata(source_root, a)
                } else {
                    TechnicalMetadata::default()
                };
                let ts = meta
                    .capture_time_utc()
                    .or_else(|| a.modified.map(DateTime::<Utc>::from))
                    .unwrap_or(ingest_ts);
                (a.clone(), meta, ts)
            })
            .collect();
        infos.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.id.cmp(&b.0.id)));

        let mut seq = profile.sequence_start;
        let mut contexts: Vec<AssetContext> = Vec::with_capacity(infos.len());
        let mut sequences = Vec::with_capacity(infos.len());
        for (asset, meta, ts) in &infos {
            let s = if asset.is_media() {
                let v = seq;
                seq += 1;
                Some(v)
            } else {
                None
            };
            sequences.push(s);
            let existing = load_source_xmp(source_root, asset, profile.write_xmp_sidecars);
            let t = &profile.iptc_template;
            let pick = |own: &Option<String>, tmpl: &Option<String>| {
                own.clone().or_else(|| tmpl.clone()).unwrap_or_default()
            };
            contexts.push(AssetContext {
                original_filename: file_name_string(&asset.primary),
                capture_timestamp: *ts,
                camera_make: meta.make.clone().unwrap_or_default(),
                camera_model: meta.model.clone().unwrap_or_default(),
                camera_serial: meta.serial.clone().unwrap_or_default(),
                iso: meta.iso.unwrap_or(0),
                lens: meta.lens.clone().unwrap_or_default(),
                shutter: meta.shutter_string(),
                aperture: meta.aperture_string(),
                focal_length: meta.focal_string(),
                video_reel: pick(&existing.meta.reel_name, &t.reel_name),
                video_scene: pick(&existing.meta.scene, &t.scene),
                video_take: pick(&existing.meta.shot_take, &t.shot_take),
                camera_angle: pick(&existing.meta.camera_angle, &t.camera_angle),
                sequence_number: s.unwrap_or(0),
                card_name: scan.card_name.clone(),
                ingest_timestamp: ingest_ts,
                extra: HashMap::new(),
            });
        }

        let mut index = TargetIndex {
            destinations: &profile.destinations,
            planned: HashSet::new(),
        };
        let mut planned_assets = Vec::new();
        let mut skipped_support = 0usize;
        let policy = profile.collision_policy;
        let card_dir_base = crate::variables::VariableEngine::evaluate_file_name(
            "{card:name}",
            &contexts.first().cloned().unwrap_or_default(),
            &profile,
        );
        let card_dir_base = if card_dir_base.is_empty() {
            "CARD".to_string()
        } else {
            card_dir_base
        };

        // ---- Preserved camera structures: one base per card -------------
        let preserved: Vec<usize> = (0..infos.len())
            .filter(|&i| profile.preserve_camera_card_structure && infos[i].0.locked)
            .collect();
        if !preserved.is_empty() {
            let first = preserved
                .iter()
                .copied()
                .find(|&i| infos[i].0.is_media())
                .unwrap_or(preserved[0]);
            let folder = VariableEngine::evaluate_path(
                &profile.folder_naming_pattern,
                &contexts[first],
                &profile,
            );
            let targets_for = |base: &str| -> Vec<PathBuf> {
                preserved
                    .iter()
                    .flat_map(|&i| copy_members(&infos[i].0, profile.write_xmp_sidecars))
                    .map(|m| join_rel(base, &m))
                    .collect()
            };
            let mut n = 1;
            let base = loop {
                let dir = if n == 1 {
                    card_dir_base.clone()
                } else {
                    format!("{card_dir_base}_{n}")
                };
                let base = if folder.is_empty() {
                    dir
                } else {
                    format!("{folder}/{dir}")
                };
                if policy != CollisionPolicy::Suffix
                    || !targets_for(&base).iter().any(|t| index.taken(t))
                {
                    break base;
                }
                n += 1;
            };
            if n > 1 {
                warnings.push(format!("card folder already present; using {base}"));
            }
            for &i in &preserved {
                let (asset, _, _) = &infos[i];
                let files = copy_members(asset, profile.write_xmp_sidecars)
                    .into_iter()
                    .map(|m| planned_file(source_root, &m, join_rel(&base, &m), policy, &mut index))
                    .collect();
                planned_assets.push(PlannedAsset {
                    asset_id: asset.id.clone(),
                    kind: asset.kind,
                    sequence: sequences[i],
                    files,
                    sidecar: (asset.is_media() && profile.write_xmp_sidecars)
                        .then(|| join_rel(&base, &asset.sidecar_rel_path())),
                    source_sidecars: source_sidecar_paths(
                        source_root,
                        asset,
                        profile.write_xmp_sidecars,
                    ),
                    context: contexts[i].clone(),
                });
            }
        }

        // ---- Everything else, asset by asset ------------------------------
        for i in 0..infos.len() {
            let (asset, _, _) = &infos[i];
            if profile.preserve_camera_card_structure && asset.locked {
                continue;
            }
            if asset.kind == AssetKind::Support {
                skipped_support += asset.members.len();
                continue;
            }
            let ctx = &contexts[i];
            let folder =
                VariableEngine::evaluate_path(&profile.folder_naming_pattern, ctx, &profile);
            let members = copy_members(asset, profile.write_xmp_sidecars);

            let (files_rel, sidecar_rel): (Vec<(PathBuf, PathBuf)>, Option<PathBuf>) = if asset
                .locked
            {
                // Names are kept; folder units keep their folder.
                let layout = |base: &str| -> (Vec<(PathBuf, PathBuf)>, PathBuf) {
                    match &asset.group_dir {
                        Some(dir) => {
                            let dir_parent = dir.parent().unwrap_or(Path::new(""));
                            let files = members
                                .iter()
                                .map(|m| {
                                    (
                                        m.clone(),
                                        join_rel(base, m.strip_prefix(dir_parent).unwrap_or(m)),
                                    )
                                })
                                .collect();
                            let side = join_rel(
                                base,
                                Path::new(&format!("{}.xmp", file_name_string(dir))),
                            );
                            (files, side)
                        }
                        None => {
                            let files = members
                                .iter()
                                .map(|m| {
                                    (m.clone(), join_rel(base, Path::new(&file_name_string(m))))
                                })
                                .collect();
                            let side = join_rel(
                                base,
                                &PathBuf::from(file_name_string(&asset.primary))
                                    .with_extension("xmp"),
                            );
                            (files, side)
                        }
                    }
                };
                let mut n = 0;
                let (files, side) = loop {
                    let base = match n {
                        0 => folder.clone(),
                        1 => join_str(&folder, &card_dir_base),
                        k => join_str(&folder, &format!("{card_dir_base}_{k}")),
                    };
                    let (files, side) = layout(&base);
                    if policy != CollisionPolicy::Suffix
                        || !files.iter().any(|(_, t)| index.taken(t))
                    {
                        break (files, side);
                    }
                    n += 1;
                };
                (files, Some(side))
            } else {
                // Renamable: one new stem for the whole group.
                let orig_stem = stem_string(&asset.primary);
                // Members share the primary's base stem (case-insensitively).
                let base_len = orig_stem.len();
                let new_stem = if profile.file_naming_pattern.trim().is_empty() || !asset.is_media()
                {
                    orig_stem.clone()
                } else {
                    let s = VariableEngine::evaluate_file_name(
                        &profile.file_naming_pattern,
                        ctx,
                        &profile,
                    );
                    if s.is_empty() {
                        orig_stem.clone()
                    } else {
                        s
                    }
                };
                let layout = |stem: &str| -> (Vec<(PathBuf, PathBuf)>, PathBuf) {
                    let files = members
                        .iter()
                        .map(|m| {
                            let name = file_name_string(m);
                            let rest = name.get(base_len..).unwrap_or("");
                            (
                                m.clone(),
                                join_rel(&folder, Path::new(&format!("{stem}{rest}"))),
                            )
                        })
                        .collect();
                    (files, join_rel(&folder, Path::new(&format!("{stem}.xmp"))))
                };
                let mut n = 0;
                let (files, side) = loop {
                    let stem = if n == 0 {
                        new_stem.clone()
                    } else {
                        format!("{new_stem}_{n}")
                    };
                    let (files, side) = layout(&stem);
                    let clash = files.iter().any(|(_, t)| index.taken(t))
                        || (profile.write_xmp_sidecars && asset.is_media() && index.planned(&side));
                    if policy != CollisionPolicy::Suffix || !clash {
                        break (files, side);
                    }
                    n += 1;
                };
                (files, Some(side))
            };

            let files: Vec<PlannedFile> = files_rel
                .into_iter()
                .map(|(src, tgt)| planned_file(source_root, &src, tgt, policy, &mut index))
                .collect();
            let sidecar = sidecar_rel.filter(|_| asset.is_media() && profile.write_xmp_sidecars);
            if let Some(s) = &sidecar {
                index.claim(s);
            }
            planned_assets.push(PlannedAsset {
                asset_id: asset.id.clone(),
                kind: asset.kind,
                sequence: sequences[i],
                files,
                sidecar,
                source_sidecars: source_sidecar_paths(
                    source_root,
                    asset,
                    profile.write_xmp_sidecars,
                ),
                context: ctx.clone(),
            });
        }

        if skipped_support > 0 {
            warnings.push(format!(
                "{skipped_support} camera-structure support file(s) skipped (structure not preserved)"
            ));
        }
        Ok(IngestPlan {
            source_root: source_root.to_path_buf(),
            card_name: scan.card_name,
            profile,
            assets: planned_assets,
            skipped_support_files: skipped_support,
            warnings,
            next_sequence: seq,
        })
    }
}

fn join_str(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{a}/{b}")
    }
}

/// Members that are copied byte-for-byte (XMP files are merged instead when
/// sidecars are enabled).
fn copy_members(asset: &Asset, write_xmp: bool) -> Vec<PathBuf> {
    asset
        .members
        .iter()
        .filter(|m| !(write_xmp && asset.is_media() && ext_of(m) == "xmp"))
        .cloned()
        .collect()
}

fn source_sidecar_paths(root: &Path, asset: &Asset, write_xmp: bool) -> Vec<PathBuf> {
    if !write_xmp || !asset.is_media() {
        return Vec::new();
    }
    let mut v: Vec<PathBuf> = asset.xmp_members().cloned().collect();
    let own = asset.sidecar_rel_path();
    if !v.contains(&own) && root.join(&own).exists() {
        v.push(own);
    }
    v
}

fn load_source_xmp(root: &Path, asset: &Asset, write_xmp: bool) -> XmpSidecar {
    let mut merged = XmpSidecar::default();
    // Highest priority last in the list; merge so later sources win.
    for p in source_sidecar_paths(root, asset, write_xmp).iter().rev() {
        if let Ok(Some(x)) = XmpSidecar::load(&root.join(p)) {
            merged.merge_from(&x);
        }
    }
    merged
}

fn planned_file(
    root: &Path,
    src: &Path,
    target: PathBuf,
    policy: CollisionPolicy,
    index: &mut TargetIndex,
) -> PlannedFile {
    let mode = if index.exists(&target) {
        match policy {
            CollisionPolicy::Overwrite => CopyMode::Overwrite,
            _ => CopyMode::KeepExisting,
        }
    } else {
        CopyMode::New
    };
    index.claim(&target);
    PlannedFile {
        source: src.to_path_buf(),
        target,
        size: std::fs::metadata(root.join(src))
            .map(|m| m.len())
            .unwrap_or(0),
        mode,
    }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub enum ProgressEvent {
    FileStarted { source: PathBuf, size: u64 },
    FileDone { source: PathBuf, bytes: u64 },
    FileSkipped { source: PathBuf },
    FileFailed { source: PathBuf, error: String },
}

pub type ProgressFn = Arc<dyn Fn(&ProgressEvent) + Send + Sync>;

#[derive(Debug, Clone, Default, Serialize)]
pub struct IngestReport {
    pub source_root: PathBuf,
    pub destinations: Vec<PathBuf>,
    pub files_copied: usize,
    pub files_kept_existing: usize,
    pub bytes_copied: u64,
    pub sidecars_written: usize,
    pub mhl_manifests: Vec<PathBuf>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub next_sequence: u64,
    pub elapsed_seconds: f64,
    /// asset id -> target paths (relative to each destination)
    pub assets: BTreeMap<String, Vec<PathBuf>>,
}

impl IngestReport {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

struct FileOutcome {
    target: PathBuf,
    source: PathBuf,
    size: u64,
    digests: HashMap<ChecksumAlgorithm, String>,
    /// Destinations that now hold a verified copy.
    dests: Vec<PathBuf>,
    copied: bool,
}

pub struct IngestExecutor;

impl IngestExecutor {
    pub async fn execute(plan: &IngestPlan, progress: Option<ProgressFn>) -> Result<IngestReport> {
        let started = std::time::Instant::now();
        let profile = &plan.profile;
        for d in &profile.destinations {
            tokio::fs::create_dir_all(d).await?;
        }
        let algos = profile.hash_algorithms();
        let verify = profile.verify_checksum;
        let sem = Arc::new(tokio::sync::Semaphore::new(
            profile.max_concurrent_files.max(1),
        ));
        let outcomes: Arc<Mutex<Vec<FileOutcome>>> = Arc::default();
        let errors: Arc<Mutex<Vec<String>>> = Arc::default();
        let mut set = tokio::task::JoinSet::new();

        for file in plan.assets.iter().flat_map(|a| a.files.iter()) {
            let permit = sem.clone().acquire_owned().await.expect("semaphore closed");
            let file = file.clone();
            let src = plan.source_root.join(&file.source);
            let dests: Vec<PathBuf> = profile.destinations.clone();
            let algos = algos.clone();
            let outcomes = outcomes.clone();
            let errors = errors.clone();
            let progress = progress.clone();
            set.spawn(async move {
                let _permit = permit;
                let emit = |e: ProgressEvent| {
                    if let Some(p) = &progress {
                        p(&e)
                    }
                };
                emit(ProgressEvent::FileStarted {
                    source: file.source.clone(),
                    size: file.size,
                });
                match Self::process_file(&src, &file, &dests, &algos, verify).await {
                    Ok(o) => {
                        emit(if o.copied {
                            ProgressEvent::FileDone {
                                source: file.source.clone(),
                                bytes: o.size,
                            }
                        } else {
                            ProgressEvent::FileSkipped {
                                source: file.source.clone(),
                            }
                        });
                        outcomes.lock().unwrap().push(o);
                    }
                    Err(e) => {
                        emit(ProgressEvent::FileFailed {
                            source: file.source.clone(),
                            error: e.to_string(),
                        });
                        errors
                            .lock()
                            .unwrap()
                            .push(format!("{}: {e}", file.source.display()));
                    }
                }
            });
        }
        while let Some(r) = set.join_next().await {
            if let Err(e) = r {
                errors.lock().unwrap().push(format!("task failed: {e}"));
            }
        }

        let outcomes = std::mem::take(&mut *outcomes.lock().unwrap());
        let mut errors = std::mem::take(&mut *errors.lock().unwrap());
        let by_source: HashMap<&Path, &FileOutcome> =
            outcomes.iter().map(|o| (o.source.as_path(), o)).collect();

        let mut report = IngestReport {
            source_root: plan.source_root.clone(),
            destinations: profile.destinations.clone(),
            warnings: plan.warnings.clone(),
            next_sequence: plan.next_sequence,
            ..Default::default()
        };
        for o in &outcomes {
            if o.copied {
                report.files_copied += 1;
                report.bytes_copied += o.size;
            } else {
                report.files_kept_existing += 1;
            }
        }

        // Sidecars.
        for pa in &plan.assets {
            report.assets.insert(
                pa.asset_id.clone(),
                pa.files.iter().map(|f| f.target.clone()).collect(),
            );
            let Some(sidecar) = &pa.sidecar else { continue };
            let primary_digest = pa
                .files
                .first()
                .and_then(|f| by_source.get(f.source.as_path()))
                .and_then(|o| {
                    o.digests
                        .get(&verify)
                        .map(|d| (verify.label().to_string(), d.clone()))
                });
            let xmp = build_sidecar(plan, pa, primary_digest);
            for d in &profile.destinations {
                let path = d.join(sidecar);
                // Keep culling done on an earlier copy of the same file.
                let mut out = xmp.clone();
                if let Ok(Some(existing)) = XmpSidecar::load(&path) {
                    out.merge_from(&existing);
                }
                match out.save(&path) {
                    Ok(()) => report.sidecars_written += 1,
                    Err(e) => errors.push(format!("sidecar {}: {e}", path.display())),
                }
            }
        }

        // ASC-MHL per destination.
        if let Some(mhl_algo) = profile.mhl_algorithm() {
            for d in &profile.destinations {
                let entries: Vec<MhlEntry> = outcomes
                    .iter()
                    .filter(|o| o.dests.contains(d))
                    .filter_map(|o| {
                        let digest = o.digests.get(&mhl_algo)?.clone();
                        let md = std::fs::metadata(d.join(&o.target)).ok();
                        Some(MhlEntry {
                            path: crate::card::path_id(&o.target),
                            size: o.size,
                            modified: md
                                .and_then(|m| m.modified().ok())
                                .map(DateTime::<Utc>::from),
                            algorithm: mhl_algo,
                            digest,
                            action: if o.copied {
                                HashAction::Original
                            } else {
                                HashAction::Verified
                            },
                        })
                    })
                    .collect();
                if entries.is_empty() {
                    continue;
                }
                match mhl::write_generation(d, &entries, MhlProcess::Transfer) {
                    Ok(p) => report.mhl_manifests.push(p),
                    Err(e) => errors.push(format!("MHL in {}: {e}", d.display())),
                }
            }
        }

        report.errors = errors;
        report.elapsed_seconds = started.elapsed().as_secs_f64();
        Ok(report)
    }

    async fn process_file(
        src: &Path,
        file: &PlannedFile,
        dest_roots: &[PathBuf],
        algos: &[ChecksumAlgorithm],
        verify: ChecksumAlgorithm,
    ) -> Result<FileOutcome> {
        // Re-check existence at execution time (the plan may be stale).
        let mut write_to = Vec::new();
        let mut keep = Vec::new();
        for d in dest_roots {
            let t = d.join(&file.target);
            if t.exists() && file.mode != CopyMode::Overwrite {
                keep.push(d.clone());
            } else {
                write_to.push(d.clone());
            }
        }
        let targets: Vec<PathBuf> = write_to.iter().map(|d| d.join(&file.target)).collect();
        let (size, digests) = if targets.is_empty() {
            let mut all = algos.to_vec();
            if verify != ChecksumAlgorithm::None && !all.contains(&verify) {
                all.push(verify);
            }
            let d = hashing::hash_file(src, &all).await?;
            (std::fs::metadata(src)?.len(), d)
        } else {
            SinglePassIngestEngine::copy_verified(src, &targets, algos, verify).await?
        };

        // Existing copies must be identical to the source.
        let cmp_algo = if verify != ChecksumAlgorithm::None {
            verify
        } else {
            ChecksumAlgorithm::XxHash64
        };
        let expected = match digests.get(&cmp_algo) {
            Some(d) => d.clone(),
            None => hashing::hash_file(src, &[cmp_algo])
                .await?
                .remove(&cmp_algo)
                .unwrap_or_default(),
        };
        for d in &keep {
            let t = d.join(&file.target);
            let actual = hashing::hash_file(&t, &[cmp_algo])
                .await?
                .remove(&cmp_algo)
                .unwrap_or_default();
            if actual != expected {
                return Err(AethrCraftError::Conflict(format!(
                    "{} exists with different content (kept, not overwritten)",
                    t.display()
                )));
            }
        }
        let mut dests = write_to;
        dests.extend(keep);
        Ok(FileOutcome {
            target: file.target.clone(),
            source: file.source.clone(),
            size,
            digests,
            copied: !targets.is_empty(),
            dests,
        })
    }
}

/// Builds the sidecar for one asset: source XMP (culling) > template.
fn build_sidecar(
    plan: &IngestPlan,
    pa: &PlannedAsset,
    checksum: Option<(String, String)>,
) -> XmpSidecar {
    let profile = &plan.profile;
    let ctx = &pa.context;
    let mut xmp = XmpSidecar::default();
    for p in pa.source_sidecars.iter().rev() {
        if let Ok(Some(x)) = XmpSidecar::load(&plan.source_root.join(p)) {
            xmp.merge_from(&x);
        }
    }
    let t = &profile.iptc_template;
    let ev = |v: &Option<String>| {
        v.as_ref()
            .map(|s| VariableEngine::evaluate(s, ctx, profile))
            .filter(|s| !s.trim().is_empty())
    };
    let template = XmpSidecar {
        meta: UnifiedMetadataTemplate {
            creator: ev(&t.creator),
            credit_line: ev(&t.credit_line),
            copyright_notice: ev(&t.copyright_notice),
            caption_abstract: ev(&t.caption_abstract),
            headline: ev(&t.headline),
            city: ev(&t.city),
            province_state: ev(&t.province_state),
            country: ev(&t.country),
            keywords: t
                .keywords
                .iter()
                .map(|k| VariableEngine::evaluate(k, ctx, profile))
                .collect(),
            reel_name: ev(&t.reel_name),
            scene: ev(&t.scene),
            shot_take: ev(&t.shot_take),
            camera_angle: ev(&t.camera_angle),
        },
        ..Default::default()
    };
    xmp.merge_from(&template);
    // Per-asset culling captions may also contain codes/tokens.
    let m = &mut xmp.meta;
    for f in [&mut m.caption_abstract, &mut m.headline] {
        if let Some(v) = f.as_mut() {
            *v = VariableEngine::evaluate(v, ctx, profile);
        }
    }
    xmp.preserved_file_name = Some(ctx.original_filename.clone());
    if checksum.is_some() {
        xmp.checksum = checksum;
    }
    xmp
}
