//! `.cube` LUT parsing and application (Resolve/Adobe format).
//!
//! Supports `LUT_1D_SIZE` (shaper) and `LUT_3D_SIZE` in the same file (the 1D
//! table is applied first), `DOMAIN_MIN`/`DOMAIN_MAX` and
//! `LUT_1D_INPUT_RANGE`/`LUT_3D_INPUT_RANGE`. Application is trilinear and
//! parallelised across image rows. LUTs are only ever applied to previews and
//! exported frames, never to the ingested originals.

use crate::error::{AethrCraftError, Result};
use image::RgbImage;
use rayon::prelude::*;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct CubeLut {
    pub title: Option<String>,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    pub shaper: Option<Vec<[f32; 3]>>,
    pub shaper_range: Option<(f32, f32)>,
    pub cube_size: usize,
    pub cube: Vec<[f32; 3]>,
}

impl CubeLut {
    pub fn load(path: &Path) -> Result<Self> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let err = |m: String| AethrCraftError::Lut(m);
        let mut title = None;
        let mut size_1d = 0usize;
        let mut size_3d = 0usize;
        let mut dmin = [0.0f32; 3];
        let mut dmax = [1.0f32; 3];
        let mut range_1d: Option<(f32, f32)> = None;
        let mut range_3d: Option<(f32, f32)> = None;
        let mut rows: Vec<[f32; 3]> = Vec::new();

        let floats = |parts: &[&str], n: usize, line: usize| -> Result<Vec<f32>> {
            if parts.len() != n {
                return Err(err(format!("line {line}: expected {n} values")));
            }
            parts
                .iter()
                .map(|p| {
                    p.parse::<f32>()
                        .map_err(|_| err(format!("line {line}: bad number {p:?}")))
                })
                .collect()
        };

        for (no, raw) in text.lines().enumerate() {
            let line = no + 1;
            let l = raw.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = l.split_whitespace().collect();
            match parts[0] {
                "TITLE" => title = Some(l[5..].trim().trim_matches('"').to_string()),
                "LUT_1D_SIZE" => size_1d = floats(&parts[1..], 1, line)?[0] as usize,
                "LUT_3D_SIZE" => size_3d = floats(&parts[1..], 1, line)?[0] as usize,
                "DOMAIN_MIN" => {
                    let v = floats(&parts[1..], 3, line)?;
                    dmin = [v[0], v[1], v[2]];
                }
                "DOMAIN_MAX" => {
                    let v = floats(&parts[1..], 3, line)?;
                    dmax = [v[0], v[1], v[2]];
                }
                "LUT_1D_INPUT_RANGE" => {
                    let v = floats(&parts[1..], 2, line)?;
                    range_1d = Some((v[0], v[1]));
                }
                "LUT_3D_INPUT_RANGE" => {
                    let v = floats(&parts[1..], 2, line)?;
                    range_3d = Some((v[0], v[1]));
                }
                k if k.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) => {
                    // Unknown keyword: ignore for forward compatibility.
                }
                _ => {
                    let v = floats(&parts, 3, line)?;
                    rows.push([v[0], v[1], v[2]]);
                }
            }
        }

        if size_3d == 0 && size_1d == 0 {
            return Err(err("missing LUT_3D_SIZE / LUT_1D_SIZE".into()));
        }
        if size_3d == 1 || size_3d > 256 || size_1d == 1 || size_1d > 65536 {
            return Err(err("LUT size out of range".into()));
        }
        let expected = size_1d + size_3d.pow(3);
        if rows.len() != expected {
            return Err(err(format!(
                "expected {expected} table rows, found {}",
                rows.len()
            )));
        }
        if let Some((a, b)) = range_3d {
            dmin = [a; 3];
            dmax = [b; 3];
        }
        if (0..3).any(|c| dmax[c] <= dmin[c]) {
            return Err(err("DOMAIN_MAX must exceed DOMAIN_MIN".into()));
        }
        let cube = rows.split_off(size_1d);
        Ok(CubeLut {
            title,
            domain_min: dmin,
            domain_max: dmax,
            shaper: (size_1d > 0).then_some(rows),
            shaper_range: if size_1d > 0 {
                Some(range_1d.unwrap_or((dmin[0], dmax[0])))
            } else {
                None
            },
            cube_size: size_3d,
            cube,
        })
    }

    /// Identity 3D LUT of the given size (useful for tests and as a template).
    pub fn identity(size: usize) -> Self {
        let mut cube = Vec::with_capacity(size.pow(3));
        let s = (size - 1) as f32;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    cube.push([r as f32 / s, g as f32 / s, b as f32 / s]);
                }
            }
        }
        CubeLut {
            title: Some("identity".into()),
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
            shaper: None,
            shaper_range: None,
            cube_size: size,
            cube,
        }
    }

    fn apply_shaper(&self, rgb: [f32; 3]) -> [f32; 3] {
        let (Some(table), Some((lo, hi))) = (&self.shaper, self.shaper_range) else {
            return rgb;
        };
        let n = table.len();
        let mut out = [0.0; 3];
        for c in 0..3 {
            let x = ((rgb[c] - lo) / (hi - lo)).clamp(0.0, 1.0) * (n - 1) as f32;
            let i = (x.floor() as usize).min(n - 2);
            let t = x - i as f32;
            out[c] = table[i][c] * (1.0 - t) + table[i + 1][c] * t;
        }
        out
    }

    /// Maps one RGB triplet (nominally 0..1).
    pub fn apply_rgb(&self, rgb: [f32; 3]) -> [f32; 3] {
        let rgb = self.apply_shaper(rgb);
        let n = self.cube_size;
        if n == 0 {
            return rgb;
        }
        let mut idx = [0usize; 3];
        let mut frac = [0f32; 3];
        for c in 0..3 {
            let x = ((rgb[c] - self.domain_min[c]) / (self.domain_max[c] - self.domain_min[c]))
                .clamp(0.0, 1.0)
                * (n - 1) as f32;
            idx[c] = (x.floor() as usize).min(n - 2);
            frac[c] = x - idx[c] as f32;
        }
        let at = |r: usize, g: usize, b: usize| self.cube[r + g * n + b * n * n];
        let lerp = |a: [f32; 3], b: [f32; 3], t: f32| {
            [
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
            ]
        };
        let (r, g, b) = (idx[0], idx[1], idx[2]);
        let c00 = lerp(at(r, g, b), at(r + 1, g, b), frac[0]);
        let c10 = lerp(at(r, g + 1, b), at(r + 1, g + 1, b), frac[0]);
        let c01 = lerp(at(r, g, b + 1), at(r + 1, g, b + 1), frac[0]);
        let c11 = lerp(at(r, g + 1, b + 1), at(r + 1, g + 1, b + 1), frac[0]);
        let c0 = lerp(c00, c10, frac[1]);
        let c1 = lerp(c01, c11, frac[1]);
        lerp(c0, c1, frac[2])
    }

    /// Grades an 8-bit RGB image in place, rows in parallel.
    pub fn apply_to_image(&self, img: &mut RgbImage) {
        let width = img.width() as usize;
        if width == 0 {
            return;
        }
        img.par_chunks_mut(width * 3).for_each(|row| {
            for px in row.chunks_exact_mut(3) {
                let out = self.apply_rgb([
                    px[0] as f32 / 255.0,
                    px[1] as f32 / 255.0,
                    px[2] as f32 / 255.0,
                ]);
                for c in 0..3 {
                    px[c] = (out[c].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        });
    }
}
