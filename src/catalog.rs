//! Drill-down catalog: a flat, filterable view over every asset on a card or
//! folder, with culling state (ratings, labels, tags, captions, markers)
//! persisted in XMP sidecars at [`Asset::sidecar_rel_path`].

use crate::card::{Asset, AssetKind, CardScan};
use crate::error::{AethrCraftError, Result};
use crate::metadata::{self, TechnicalMetadata};
use crate::xmp::XmpSidecar;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub asset: Asset,
    pub sidecar_path: PathBuf,
    pub xmp: XmpSidecar,
    pub metadata: TechnicalMetadata,
}

impl CatalogEntry {
    pub fn save(&self) -> Result<()> {
        self.xmp.save(&self.sidecar_path)
    }
}

/// Filter for the flattened view. Empty fields match everything.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub kinds: Vec<AssetKind>,
    pub min_rating: Option<i8>,
    pub label: Option<String>,
    pub tagged_only: bool,
    /// Case-insensitive substring over name, caption, headline and keywords.
    pub text: Option<String>,
    pub extensions: Vec<String>,
    pub include_support: bool,
}

impl Filter {
    pub fn matches(&self, e: &CatalogEntry) -> bool {
        let a = &e.asset;
        if !self.include_support && matches!(a.kind, AssetKind::Support) {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&a.kind) {
            return false;
        }
        if let Some(min) = self.min_rating {
            if e.xmp.rating.unwrap_or(0) < min {
                return false;
            }
        }
        if let Some(l) = &self.label {
            if !e
                .xmp
                .label
                .as_deref()
                .is_some_and(|x| x.eq_ignore_ascii_case(l))
            {
                return false;
            }
        }
        if self.tagged_only && !e.xmp.tagged {
            return false;
        }
        if !self.extensions.is_empty() {
            let ext = crate::card::ext_of(&a.primary);
            if !self
                .extensions
                .iter()
                .any(|x| x.trim_start_matches('.').eq_ignore_ascii_case(&ext))
            {
                return false;
            }
        }
        if let Some(t) = &self.text {
            let t = t.to_lowercase();
            let m = &e.xmp.meta;
            let hay = [
                a.id.to_lowercase(),
                m.caption_abstract
                    .clone()
                    .unwrap_or_default()
                    .to_lowercase(),
                m.headline.clone().unwrap_or_default().to_lowercase(),
                m.keywords.join(" ").to_lowercase(),
            ];
            if !hay.iter().any(|h| h.contains(&t)) {
                return false;
            }
        }
        true
    }
}

pub struct Catalog {
    pub scan: CardScan,
    pub entries: Vec<CatalogEntry>,
}

impl Catalog {
    /// Scans `root`, loads sidecars and (optionally) technical metadata.
    pub fn open(root: &Path, read_metadata: bool) -> Result<Catalog> {
        let scan = CardScan::scan(root)?;
        let mut entries = Vec::with_capacity(scan.assets.len());
        for asset in &scan.assets {
            let sidecar_path = root.join(asset.sidecar_rel_path());
            let xmp = XmpSidecar::load(&sidecar_path)?.unwrap_or_default();
            let metadata = if read_metadata && asset.is_media() {
                read_asset_metadata(root, asset)
            } else {
                TechnicalMetadata::default()
            };
            entries.push(CatalogEntry {
                asset: asset.clone(),
                sidecar_path,
                xmp,
                metadata,
            });
        }
        Ok(Catalog { scan, entries })
    }

    pub fn filter<'a>(&'a self, f: &'a Filter) -> impl Iterator<Item = &'a CatalogEntry> + 'a {
        self.entries.iter().filter(move |e| f.matches(e))
    }

    /// Finds entries by exact id/member path, or by case-insensitive substring
    /// of the id when `fuzzy` is set.
    pub fn select_mut(&mut self, selector: &str, fuzzy: bool) -> Result<Vec<&mut CatalogEntry>> {
        let norm = selector.replace('\\', "/");
        let exact: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.asset.id == norm
                    || e.asset
                        .members
                        .iter()
                        .any(|m| crate::card::path_id(m) == norm)
            })
            .map(|(i, _)| i)
            .collect();
        let idx = if !exact.is_empty() || !fuzzy {
            exact
        } else {
            let l = norm.to_lowercase();
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.asset.is_media() && e.asset.id.to_lowercase().contains(&l))
                .map(|(i, _)| i)
                .collect()
        };
        if idx.is_empty() {
            return Err(AethrCraftError::NotFound(format!(
                "no asset matches {selector:?}"
            )));
        }
        Ok(self
            .entries
            .iter_mut()
            .enumerate()
            .filter(|(i, _)| idx.contains(i))
            .map(|(_, e)| e)
            .collect())
    }
}

/// Reads metadata for an asset: the primary file first, then any member that
/// can fill gaps (Sony `M01.XML`, `.THM`, paired JPEG...). For folder units
/// the first frame/segment is used.
pub fn read_asset_metadata(root: &Path, asset: &Asset) -> TechnicalMetadata {
    let mut meta = metadata::read_metadata(&root.join(&asset.primary)).unwrap_or_default();
    // Preview ranges refer to the primary file only.
    for m in asset.members.iter().skip(1) {
        let ext = crate::card::ext_of(m);
        if matches!(ext.as_str(), "xml" | "jpg" | "jpeg" | "thm") {
            if let Ok(mut other) = metadata::read_metadata(&root.join(m)) {
                other.preview_ranges.clear();
                meta.fill_from(other);
            }
        }
    }
    meta
}

/// Best image source for previewing an asset: thumbnails shipped by the
/// camera (`THMBNL/*.JPG`, `.THM`) for video, otherwise the primary file.
pub fn preview_source(asset: &Asset) -> PathBuf {
    if asset.kind == AssetKind::Video {
        if let Some(t) = asset.members.iter().find(|m| {
            matches!(
                crate::card::ext_of(m).as_str(),
                "jpg" | "jpeg" | "thm" | "bmp"
            )
        }) {
            return t.clone();
        }
    }
    asset.primary.clone()
}
