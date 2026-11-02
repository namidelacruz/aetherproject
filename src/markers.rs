//! Frame rates, SMPTE timecode (incl. drop-frame), markers and subclips.
//!
//! Markers are stored frame-accurately relative to the clip start, so the
//! same data drives XMP sidecars, FCPXML and ALE without rounding drift.

use crate::error::{AethrCraftError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl FrameRate {
    pub const FPS_25: FrameRate = FrameRate { num: 25, den: 1 };

    /// Parses `25`, `29.97`, `23.976`, `59.94`, `24000/1001`, `25p`...
    pub fn parse(s: &str) -> Result<FrameRate> {
        let s = s.trim().trim_end_matches(['p', 'P', 'i', 'I']);
        if let Some((n, d)) = s.split_once('/') {
            let (n, d) = (n.trim().parse::<u32>(), d.trim().parse::<u32>());
            if let (Ok(num), Ok(den)) = (n, d) {
                if num > 0 && den > 0 {
                    return Ok(FrameRate { num, den });
                }
            }
            return Err(AethrCraftError::Timecode(format!("bad frame rate {s:?}")));
        }
        let f: f64 = s
            .parse()
            .map_err(|_| AethrCraftError::Timecode(format!("bad frame rate {s:?}")))?;
        Self::from_f64(f)
    }

    pub fn from_f64(f: f64) -> Result<FrameRate> {
        if !(f > 0.0 && f <= 1000.0) {
            return Err(AethrCraftError::Timecode(format!(
                "frame rate {f} out of range"
            )));
        }
        let rounded = f.round();
        if (f - rounded).abs() < 0.001 {
            return Ok(FrameRate {
                num: rounded as u32,
                den: 1,
            });
        }
        let ntsc = rounded * 1000.0 / 1001.0;
        if (f - ntsc).abs() < 0.01 {
            return Ok(FrameRate {
                num: rounded as u32 * 1000,
                den: 1001,
            });
        }
        Ok(FrameRate {
            num: (f * 1000.0).round() as u32,
            den: 1000,
        })
    }

    pub fn fps(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// Integer timecode base (30 for 29.97).
    pub fn timebase(self) -> u32 {
        self.fps().round() as u32
    }

    pub fn supports_drop_frame(self) -> bool {
        self.den == 1001 && self.timebase() % 30 == 0
    }

    pub fn label(self) -> String {
        if self.den == 1 {
            self.num.to_string()
        } else {
            format!("{:.3}", self.fps())
                .trim_end_matches('0')
                .to_string()
        }
    }

    pub fn frames_to_seconds(self, frames: u64) -> f64 {
        frames as f64 * self.den as f64 / self.num as f64
    }

    pub fn seconds_to_frames(self, secs: f64) -> u64 {
        (secs * self.fps()).round().max(0.0) as u64
    }
}

impl Default for FrameRate {
    fn default() -> Self {
        Self::FPS_25
    }
}

/// Parses `HH:MM:SS:FF` (`;` before FF = drop-frame) or plain seconds (`12.5s`).
/// Returns `(frame_count, is_drop_frame)`.
pub fn parse_timecode(s: &str, rate: FrameRate) -> Result<(u64, bool)> {
    let s = s.trim();
    if let Some(secs) = s.strip_suffix('s') {
        let v: f64 = secs
            .parse()
            .map_err(|_| AethrCraftError::Timecode(s.into()))?;
        return Ok((rate.seconds_to_frames(v), false));
    }
    let drop = s.contains(';');
    let parts: Vec<&str> = s.split([':', ';', '.']).collect();
    if parts.len() != 4 {
        return Err(AethrCraftError::Timecode(format!(
            "expected HH:MM:SS:FF, got {s:?}"
        )));
    }
    let n: Vec<u64> = parts
        .iter()
        .map(|p| {
            p.parse::<u64>()
                .map_err(|_| AethrCraftError::Timecode(s.into()))
        })
        .collect::<Result<_>>()?;
    let (hh, mm, ss, ff) = (n[0], n[1], n[2], n[3]);
    let tb = rate.timebase() as u64;
    if mm >= 60 || ss >= 60 || ff >= tb {
        return Err(AethrCraftError::Timecode(format!(
            "{s:?} out of range for {} fps",
            rate.label()
        )));
    }
    if drop && !rate.supports_drop_frame() {
        return Err(AethrCraftError::Timecode(format!(
            "drop-frame not valid at {} fps",
            rate.label()
        )));
    }
    let mut frames = ((hh * 3600 + mm * 60 + ss) * tb) + ff;
    if drop {
        let drop_per_min = 2 * (tb / 30);
        let total_min = hh * 60 + mm;
        if ss == 0 && ff < drop_per_min && mm % 10 != 0 {
            return Err(AethrCraftError::Timecode(format!(
                "{s:?} does not exist in drop-frame"
            )));
        }
        frames -= drop_per_min * (total_min - total_min / 10);
    }
    Ok((frames, drop))
}

/// Formats a frame count as SMPTE timecode.
pub fn format_timecode(frames: u64, rate: FrameRate, drop: bool) -> String {
    let tb = rate.timebase() as u64;
    let mut f = frames;
    let drop = drop && rate.supports_drop_frame();
    if drop {
        let d = 2 * (tb / 30);
        let per_10 = tb * 600 - d * 9;
        let per_min = tb * 60 - d;
        let tens = f / per_10;
        let rem = f % per_10;
        f += d * 9 * tens
            + if rem > d {
                d * ((rem - d) / per_min)
            } else {
                0
            };
    }
    let ff = f % tb;
    let total_s = f / tb;
    let sep = if drop { ';' } else { ':' };
    format!(
        "{:02}:{:02}:{:02}{}{:02}",
        total_s / 3600,
        (total_s / 60) % 60,
        total_s % 60,
        sep,
        ff
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub name: String,
    #[serde(default)]
    pub note: String,
    /// Frames from clip start.
    pub start: u64,
    /// 0 for a point marker.
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub color: Option<String>,
    /// In/Out range exported as a subclip.
    #[serde(default)]
    pub subclip: bool,
}

impl Marker {
    pub fn end(&self) -> u64 {
        self.start + self.duration
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkerSet {
    pub rate: FrameRate,
    pub markers: Vec<Marker>,
}

impl MarkerSet {
    /// Adds a marker; a subclip must have a non-empty range. Markers are kept
    /// sorted; a marker with the same name and start replaces the old one.
    pub fn add(&mut self, m: Marker) -> Result<()> {
        if m.subclip && m.duration == 0 {
            return Err(AethrCraftError::Timecode(
                "subclip needs an Out point after its In point".into(),
            ));
        }
        self.markers
            .retain(|x| !(x.name == m.name && x.start == m.start));
        self.markers.push(m);
        self.markers.sort_by_key(|x| (x.start, x.duration));
        Ok(())
    }

    pub fn subclips(&self) -> impl Iterator<Item = &Marker> {
        self.markers.iter().filter(|m| m.subclip)
    }
}
