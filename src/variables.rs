//! Unified `{token}` variable and `\code\` replacement engine.

use crate::profile::UnifiedIngestProfile;
use chrono::{DateTime, Utc};
use regex::Regex;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct AssetContext {
    pub original_filename: String,
    pub capture_timestamp: DateTime<Utc>,
    pub camera_model: String,
    pub camera_serial: String,
    pub iso: u32,
    pub lens: String,
    pub video_reel: String,
    pub video_scene: String,
    pub video_take: String,
    pub sequence_number: u64,
}

pub struct VariableEngine;

impl VariableEngine {
    /// Expands unified `{tokens}` and `\code\` replacements.
    /// Supports indexed column selection such as `\code#2\` (1-based).
    pub fn evaluate(template: &str, ctx: &AssetContext, profile: &UnifiedIngestProfile) -> String {
        let ts = &ctx.capture_timestamp;
        let file_stem = Path::new(&ctx.original_filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("clip");

        // Step 1: unified tokens.
        let output = template
            .replace("{date:YYYYMMDD}", &ts.format("%Y%m%d").to_string())
            .replace("{date:YYYY}", &ts.format("%Y").to_string())
            .replace("{date:MM}", &ts.format("%m").to_string())
            .replace("{date:DD}", &ts.format("%d").to_string())
            .replace(
                "{seq:4}",
                &format!(
                    "{:0width$}",
                    ctx.sequence_number,
                    width = profile.sequence_padding
                ),
            )
            .replace("{camera:model}", &ctx.camera_model)
            .replace("{camera:serial}", &ctx.camera_serial)
            .replace("{exif:iso}", &ctx.iso.to_string())
            .replace("{exif:lens}", &ctx.lens)
            .replace("{video:reel}", &ctx.video_reel)
            .replace("{video:scene}", &ctx.video_scene)
            .replace("{video:take}", &ctx.video_take)
            .replace("{file:stem}", file_stem);

        // Step 2: code replacements, e.g. `\p10\` or `\p10#2\`.
        let delim = regex::escape(&profile.code_replacement_delimiter.to_string());
        let pattern = format!(r"{d}([^{d}#]+)(?:#(\d+))?{d}", d = delim);
        let Ok(re) = Regex::new(&pattern) else {
            return output;
        };

        re.replace_all(&output, |caps: &regex::Captures| {
            let key = &caps[1];
            let col = caps
                .get(2)
                .and_then(|m| m.as_str().parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1);

            match profile.code_replacements.get(key) {
                Some(cols) if !cols.is_empty() => cols.get(col).unwrap_or(&cols[0]).clone(),
                // Unknown keys are left unexpanded.
                _ => caps[0].to_string(),
            }
        })
        .into_owned()
    }
}
