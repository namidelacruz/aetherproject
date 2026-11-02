//! `aethrcraft` command-line interface.

use aethrcraft::card::AssetKind;
use aethrcraft::catalog::{Catalog, Filter};
use aethrcraft::importers::{ImportSources, SettingsImporter};
use aethrcraft::ingest::{IngestExecutor, IngestPlanner, ProgressEvent};
use aethrcraft::lut::CubeLut;
use aethrcraft::markers::{format_timecode, parse_timecode, FrameRate, Marker};
use aethrcraft::preview::{export_video_frame, PreviewOptions, PreviewRenderer};
use aethrcraft::profile::{ChecksumAlgorithm, CollisionPolicy, UnifiedIngestProfile};
use aethrcraft::variables::{AssetContext, VariableEngine};
use aethrcraft::{hashing, mhl, nle, AethrCraftError, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "aethrcraft",
    version,
    about = "AethrCraft — media offload, culling & metadata engine"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create, import, inspect and validate ingest profiles.
    #[command(subcommand)]
    Profile(ProfileCmd),
    /// Detect camera structures and list assets (flat drill-down view).
    Scan(ScanArgs),
    /// Offload a card to every destination with verification, sidecars and ASC-MHL.
    Ingest(IngestArgs),
    /// Re-verify a destination against its ASC-MHL history.
    Verify {
        root: PathBuf,
        /// Do not record the verification as a new MHL generation.
        #[arg(long)]
        no_record: bool,
    },
    /// Hash files with one or more algorithms.
    Hash {
        files: Vec<PathBuf>,
        #[arg(long, value_delimiter = ',', default_value = "xxh64")]
        algo: Vec<String>,
    },
    /// Render a JPEG preview (embedded RAW preview, optional LUT).
    Preview {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        lut: Option<PathBuf>,
        #[arg(long, default_value_t = 2048)]
        max_size: u32,
        #[arg(long, default_value_t = 88)]
        quality: u8,
    },
    /// Export one video frame (requires ffmpeg), optionally through a LUT.
    Frame {
        video: PathBuf,
        /// Timecode HH:MM:SS:FF or seconds like 12.5s
        #[arg(long)]
        at: String,
        #[arg(long, default_value = "25")]
        fps: String,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        lut: Option<PathBuf>,
        #[arg(long, default_value_t = 0)]
        max_size: u32,
    },
    /// Apply a .cube LUT to an image file.
    Lut {
        image: PathBuf,
        #[arg(long)]
        lut: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Rate, label, tag and caption assets (written to XMP sidecars).
    Cull(CullArgs),
    /// Add markers or In/Out subclips to an asset.
    Mark(MarkArgs),
    /// Expand a template against a file (or sample data) for testing.
    Expand {
        template: String,
        #[arg(long)]
        profile: Option<PathBuf>,
        /// Card root containing the file, plus the file (asset id).
        #[arg(long, requires = "id")]
        root: Option<PathBuf>,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        path: bool,
    },
    /// Export catalog metadata to an NLE format.
    Export {
        root: PathBuf,
        #[arg(long, value_enum)]
        format: NleFormat,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, default_value = "25")]
        fps: String,
        #[arg(long)]
        event: Option<String>,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Import NLE metadata (ALE or FCPXML) back into sidecars.
    ImportNle {
        root: PathBuf,
        file: PathBuf,
        #[arg(long, default_value = "25")]
        fps: String,
    },
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// Write a default profile.
    Init {
        #[arg(short, long, default_value = "aethrcraft.toml")]
        output: PathBuf,
        #[arg(long)]
        dest: Vec<PathBuf>,
    },
    /// Combine legacy settings into one native profile.
    Import {
        #[arg(long)]
        base: Option<PathBuf>,
        #[arg(long)]
        pmx: Option<PathBuf>,
        #[arg(long)]
        snap: Option<PathBuf>,
        #[arg(long)]
        codes: Option<PathBuf>,
        #[arg(long)]
        kyno: Option<PathBuf>,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Print a profile and its validation warnings.
    Show { profile: PathBuf },
}

#[derive(Args, Clone, Default)]
struct FilterArgs {
    #[arg(long, value_enum)]
    kind: Vec<KindArg>,
    #[arg(long)]
    min_rating: Option<i8>,
    #[arg(long)]
    label: Option<String>,
    #[arg(long)]
    tagged: bool,
    #[arg(long)]
    text: Option<String>,
    #[arg(long = "ext")]
    extensions: Vec<String>,
    #[arg(long)]
    include_support: bool,
}

impl FilterArgs {
    fn to_filter(&self) -> Filter {
        Filter {
            kinds: self.kind.iter().map(|k| k.kind()).collect(),
            min_rating: self.min_rating,
            label: self.label.clone(),
            tagged_only: self.tagged,
            text: self.text.clone(),
            extensions: self.extensions.clone(),
            include_support: self.include_support,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum KindArg {
    Video,
    Still,
    Audio,
    Other,
    Support,
}

impl KindArg {
    fn kind(self) -> AssetKind {
        match self {
            KindArg::Video => AssetKind::Video,
            KindArg::Still => AssetKind::Still,
            KindArg::Audio => AssetKind::Audio,
            KindArg::Other => AssetKind::Other,
            KindArg::Support => AssetKind::Support,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum NleFormat {
    Fcpxml,
    Ale,
    Csv,
}

#[derive(Args)]
struct ScanArgs {
    root: PathBuf,
    #[arg(long)]
    json: bool,
    /// Skip technical metadata (faster on huge cards).
    #[arg(long)]
    no_metadata: bool,
    #[command(flatten)]
    filter: FilterArgs,
}

#[derive(Args)]
struct IngestArgs {
    source: PathBuf,
    #[arg(short, long)]
    profile: Option<PathBuf>,
    /// Destinations (override the profile's list).
    #[arg(long)]
    dest: Vec<PathBuf>,
    #[arg(long)]
    verify: Option<String>,
    #[arg(long, value_enum)]
    collision: Option<CollisionArg>,
    #[arg(long)]
    no_mhl: bool,
    #[arg(long)]
    no_sidecars: bool,
    #[arg(long)]
    flatten: bool,
    /// Show the plan without copying.
    #[arg(long)]
    dry_run: bool,
    /// Write a JSON report here.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Store the next sequence number back into the profile file.
    #[arg(long)]
    save_sequence: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum CollisionArg {
    Suffix,
    Skip,
    Overwrite,
}

#[derive(Args)]
struct CullArgs {
    root: PathBuf,
    /// Asset ids / member paths, or substrings with --fuzzy.
    #[arg(required = true)]
    select: Vec<String>,
    #[arg(long)]
    fuzzy: bool,
    #[arg(long, allow_hyphen_values = true)]
    rating: Option<i8>,
    #[arg(long)]
    label: Option<String>,
    #[arg(long)]
    tag: bool,
    #[arg(long)]
    untag: bool,
    #[arg(long)]
    keyword: Vec<String>,
    #[arg(long)]
    remove_keyword: Vec<String>,
    #[arg(long)]
    caption: Option<String>,
    #[arg(long)]
    headline: Option<String>,
    #[arg(long)]
    reel: Option<String>,
    #[arg(long)]
    scene: Option<String>,
    #[arg(long)]
    take: Option<String>,
    #[arg(long)]
    angle: Option<String>,
    /// Profile whose code replacements expand in --caption / --headline.
    #[arg(long)]
    profile: Option<PathBuf>,
}

#[derive(Args)]
struct MarkArgs {
    root: PathBuf,
    select: String,
    #[arg(long, required_unless_present = "list")]
    name: Option<String>,
    #[arg(long = "in", required_unless_present = "list")]
    in_point: Option<String>,
    #[arg(long = "out")]
    out_point: Option<String>,
    #[arg(long, default_value = "")]
    note: String,
    #[arg(long)]
    color: Option<String>,
    #[arg(long, default_value = "25")]
    fps: String,
    /// Export this In/Out range as a subclip.
    #[arg(long, requires = "out_point")]
    subclip: bool,
    /// List markers instead of adding.
    #[arg(long)]
    list: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn load_profile(path: Option<&Path>) -> Result<UnifiedIngestProfile> {
    match path {
        Some(p) => SettingsImporter::import_file(p),
        None => Ok(UnifiedIngestProfile::default()),
    }
}

fn human_bytes(b: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", units[i])
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Profile(cmd) => profile_cmd(cmd)?,
        Command::Scan(a) => scan_cmd(a)?,
        Command::Ingest(a) => return ingest_cmd(a).await,
        Command::Verify { root, no_record } => {
            let r = mhl::verify(&root, !no_record).await?;
            println!(
                "verified: {}  failed: {}  missing: {}",
                r.verified.len(),
                r.failed.len(),
                r.missing.len()
            );
            for (p, exp, act) in &r.failed {
                println!("FAILED  {p}  expected {exp} got {act}");
            }
            for p in &r.missing {
                println!("MISSING {p}");
            }
            if let Some(g) = &r.generation {
                println!("recorded {}", g.display());
            }
            if !r.ok() {
                return Ok(ExitCode::FAILURE);
            }
        }
        Command::Hash { files, algo } => {
            let algos: Vec<ChecksumAlgorithm> = algo
                .iter()
                .map(|a| {
                    ChecksumAlgorithm::from_name(a).ok_or_else(|| {
                        AethrCraftError::SchemaError(format!("unknown algorithm {a}"))
                    })
                })
                .collect::<Result<_>>()?;
            for f in files {
                let d = hashing::hash_file(&f, &algos).await?;
                for a in &algos {
                    println!(
                        "{}  {}  {}",
                        a.label(),
                        d.get(a).map(String::as_str).unwrap_or("-"),
                        f.display()
                    );
                }
            }
        }
        Command::Preview {
            file,
            output,
            lut,
            max_size,
            quality,
        } => {
            let lut = lut.map(|p| CubeLut::load(&p)).transpose()?;
            let opts = PreviewOptions {
                max_size,
                quality,
                ..Default::default()
            };
            let jpeg = PreviewRenderer::render(&file, lut.as_ref(), &opts)?;
            std::fs::write(&output, jpeg)?;
            println!("wrote {}", output.display());
        }
        Command::Frame {
            video,
            at,
            fps,
            output,
            lut,
            max_size,
        } => {
            let rate = FrameRate::parse(&fps)?;
            let (frames, _) = parse_timecode(&at, rate)?;
            let lut = lut.map(|p| CubeLut::load(&p)).transpose()?;
            let opts = PreviewOptions {
                max_size,
                ..Default::default()
            };
            export_video_frame(
                &video,
                rate.frames_to_seconds(frames),
                &output,
                lut.as_ref(),
                &opts,
            )?;
            println!("wrote {}", output.display());
        }
        Command::Lut { image, lut, output } => {
            let lut = CubeLut::load(&lut)?;
            let mut img = image::open(&image)?.to_rgb8();
            lut.apply_to_image(&mut img);
            img.save(&output)?;
            println!("wrote {}", output.display());
        }
        Command::Cull(a) => cull_cmd(a)?,
        Command::Mark(a) => mark_cmd(a)?,
        Command::Expand {
            template,
            profile,
            root,
            id,
            path,
        } => {
            let profile = load_profile(profile.as_deref())?;
            let ctx = match (root, id) {
                (Some(root), Some(id)) => context_for(&root, &id, &profile)?,
                _ => sample_context(),
            };
            let out = if path {
                VariableEngine::evaluate_path(&template, &ctx, &profile)
            } else {
                VariableEngine::evaluate(&template, &ctx, &profile)
            };
            println!("{out}");
            for t in VariableEngine::unknown_tokens(&template) {
                eprintln!("warning: unknown token {t}");
            }
        }
        Command::Export {
            root,
            format,
            output,
            fps,
            event,
            filter,
        } => {
            let rate = FrameRate::parse(&fps)?;
            let cat = Catalog::open(&root, true)?;
            let f = Filter {
                kinds: if filter.kind.is_empty() {
                    vec![AssetKind::Video, AssetKind::Still, AssetKind::Audio]
                } else {
                    filter.to_filter().kinds
                },
                ..filter.to_filter()
            };
            let entries: Vec<_> = cat.filter(&f).collect();
            let text = match format {
                NleFormat::Fcpxml => nle::export_fcpxml(
                    &cat,
                    &entries,
                    event.as_deref().unwrap_or(&cat.scan.card_name),
                    rate,
                ),
                NleFormat::Ale => nle::export_ale(&cat, &entries, rate),
                NleFormat::Csv => nle::export_csv(&entries),
            };
            std::fs::write(&output, text)?;
            println!(
                "exported {} asset(s) to {}",
                entries.len(),
                output.display()
            );
        }
        Command::ImportNle { root, file, fps } => {
            let rate = FrameRate::parse(&fps)?;
            let text = std::fs::read_to_string(&file)?;
            let clips = if text.trim_start().starts_with('<') {
                nle::parse_fcpxml(&text, rate)
            } else {
                nle::parse_ale(&text)
            };
            let mut cat = Catalog::open(&root, false)?;
            let n = nle::apply_import(&mut cat, &clips, rate)?;
            println!("updated {n} of {} clip(s)", clips.len());
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn profile_cmd(cmd: ProfileCmd) -> Result<()> {
    match cmd {
        ProfileCmd::Init { output, dest } => {
            let p = UnifiedIngestProfile {
                destinations: dest,
                ..Default::default()
            };
            p.save(&output)?;
            println!("wrote {}", output.display());
        }
        ProfileCmd::Import {
            base,
            pmx,
            snap,
            codes,
            kyno,
            output,
        } => {
            let mut p = SettingsImporter::combine(&ImportSources {
                native: base,
                kyno,
                pmx,
                snapshot: snap,
                codes,
            })?;
            for w in p.validate()? {
                eprintln!("warning: {w}");
            }
            p.save(&output)?;
            println!(
                "wrote {} ({} code replacement(s))",
                output.display(),
                p.code_replacements.len()
            );
        }
        ProfileCmd::Show { profile } => {
            let mut p = SettingsImporter::import_file(&profile)?;
            let warnings = p.validate()?;
            print!("{}", toml::to_string_pretty(&p)?);
            for w in warnings {
                eprintln!("warning: {w}");
            }
        }
    }
    Ok(())
}

fn scan_cmd(a: ScanArgs) -> Result<()> {
    let cat = Catalog::open(&a.root, !a.no_metadata)?;
    let f = a.filter.to_filter();
    let entries: Vec<_> = cat.filter(&f).collect();
    if a.json {
        #[derive(serde::Serialize)]
        struct Row<'a> {
            asset: &'a aethrcraft::Asset,
            metadata: &'a aethrcraft::TechnicalMetadata,
            rating: Option<i8>,
            label: Option<&'a str>,
            tagged: bool,
            keywords: &'a [String],
            markers: &'a [Marker],
        }
        let rows: Vec<Row> = entries
            .iter()
            .map(|e| Row {
                asset: &e.asset,
                metadata: &e.metadata,
                rating: e.xmp.rating,
                label: e.xmp.label.as_deref(),
                tagged: e.xmp.tagged,
                keywords: &e.xmp.meta.keywords,
                markers: &e.xmp.markers.markers,
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "root": cat.scan.root,
                "card_name": cat.scan.card_name,
                "structures": cat.scan.structures,
                "assets": rows,
            }))?
        );
        return Ok(());
    }
    println!(
        "Card: {}  ({})",
        cat.scan.card_name,
        cat.scan.root.display()
    );
    for s in &cat.scan.structures {
        println!("  structure {:?} at {}", s.format, s.root.display());
    }
    for e in &entries {
        let a = &e.asset;
        let stars = match e.xmp.rating {
            Some(-1) => "reject".to_string(),
            Some(r) if r > 0 => "*".repeat(r as usize),
            _ => String::new(),
        };
        println!(
            "{:<7} {:<5} {:>10}  {:<48} {:<16} {:<19} {} {}{}",
            format!("{:?}", a.kind),
            if a.locked { "lock" } else { "" },
            human_bytes(a.total_size),
            a.id,
            e.metadata.model.as_deref().unwrap_or(""),
            e.metadata
                .capture_time
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default(),
            stars,
            e.xmp.label.as_deref().unwrap_or(""),
            if e.xmp.tagged { " [tagged]" } else { "" },
        );
        if a.members.len() > 1 {
            println!("{:>26}+ {} member file(s)", "", a.members.len() - 1);
        }
    }
    println!("{} asset(s)", entries.len());
    Ok(())
}

async fn ingest_cmd(a: IngestArgs) -> Result<ExitCode> {
    let mut profile = load_profile(a.profile.as_deref())?;
    if !a.dest.is_empty() {
        profile.destinations = a.dest.clone();
    }
    if let Some(v) = &a.verify {
        profile.verify_checksum = ChecksumAlgorithm::from_name(v)
            .ok_or_else(|| AethrCraftError::SchemaError(format!("unknown algorithm {v}")))?;
    }
    if let Some(c) = a.collision {
        profile.collision_policy = match c {
            CollisionArg::Suffix => CollisionPolicy::Suffix,
            CollisionArg::Skip => CollisionPolicy::Skip,
            CollisionArg::Overwrite => CollisionPolicy::Overwrite,
        };
    }
    if a.no_mhl {
        profile.generate_asc_mhl = false;
    }
    if a.no_sidecars {
        profile.write_xmp_sidecars = false;
    }
    if a.flatten {
        profile.preserve_camera_card_structure = false;
    }

    let plan = IngestPlanner::plan(&a.source, &profile)?;
    for w in &plan.warnings {
        eprintln!("warning: {w}");
    }
    println!(
        "{} asset(s), {} file(s), {} -> {} destination(s)",
        plan.assets.len(),
        plan.file_count(),
        human_bytes(plan.total_bytes()),
        plan.profile.destinations.len()
    );
    if a.dry_run {
        for pa in &plan.assets {
            for f in &pa.files {
                println!(
                    "  {:?}  {}  ->  {}",
                    f.mode,
                    f.source.display(),
                    f.target.display()
                );
            }
            if let Some(s) = &pa.sidecar {
                println!("  Sidecar  ->  {}", s.display());
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    let progress: aethrcraft::ingest::ProgressFn = Arc::new(|e: &ProgressEvent| match e {
        ProgressEvent::FileDone { source, bytes } => {
            println!("  ok    {} ({})", source.display(), human_bytes(*bytes))
        }
        ProgressEvent::FileSkipped { source } => println!("  kept  {}", source.display()),
        ProgressEvent::FileFailed { source, error } => {
            eprintln!("  FAIL  {}: {error}", source.display())
        }
        ProgressEvent::FileStarted { .. } => {}
    });
    let report = IngestExecutor::execute(&plan, Some(progress)).await?;
    println!(
        "copied {} file(s) ({}), kept {}, {} sidecar(s), {} MHL manifest(s) in {:.1}s",
        report.files_copied,
        human_bytes(report.bytes_copied),
        report.files_kept_existing,
        report.sidecars_written,
        report.mhl_manifests.len(),
        report.elapsed_seconds
    );
    for e in &report.errors {
        eprintln!("error: {e}");
    }
    if let Some(r) = &a.report {
        std::fs::write(r, serde_json::to_string_pretty(&report)?)?;
    }
    if a.save_sequence && report.ok() {
        if let Some(p) = &a.profile {
            let mut stored =
                UnifiedIngestProfile::load(p).or_else(|_| SettingsImporter::import_file(p))?;
            stored.sequence_start = report.next_sequence;
            let target = if matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("toml") | Some("json")
            ) {
                p.clone()
            } else {
                p.with_extension("toml")
            };
            stored.save(&target)?;
            println!(
                "next sequence {} saved to {}",
                report.next_sequence,
                target.display()
            );
        }
    }
    Ok(if report.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn cull_cmd(a: CullArgs) -> Result<()> {
    if a.tag && a.untag {
        return Err(AethrCraftError::SchemaError(
            "--tag and --untag are mutually exclusive".into(),
        ));
    }
    if let Some(r) = a.rating {
        if !(-1..=5).contains(&r) {
            return Err(AethrCraftError::SchemaError(
                "rating must be -1 (reject) to 5".into(),
            ));
        }
    }
    let profile = load_profile(a.profile.as_deref())?;
    let mut cat = Catalog::open(&a.root, false)?;
    let mut count = 0;
    for sel in &a.select {
        for e in cat.select_mut(sel, a.fuzzy)? {
            let x = &mut e.xmp;
            if let Some(r) = a.rating {
                x.rating = Some(r);
            }
            if let Some(l) = &a.label {
                x.label = (!l.is_empty()).then(|| l.clone());
            }
            if a.tag {
                x.tagged = true;
            }
            if a.untag {
                x.tagged = false;
            }
            let m = &mut x.meta;
            for k in &a.keyword {
                if !m.keywords.iter().any(|x| x.eq_ignore_ascii_case(k)) {
                    m.keywords.push(k.clone());
                }
            }
            m.keywords
                .retain(|k| !a.remove_keyword.iter().any(|r| r.eq_ignore_ascii_case(k)));
            // Codes expand now; {tokens} stay for ingest-time evaluation.
            if let Some(c) = &a.caption {
                m.caption_abstract = Some(VariableEngine::expand_codes(c, &profile));
            }
            if let Some(h) = &a.headline {
                m.headline = Some(VariableEngine::expand_codes(h, &profile));
            }
            for (field, v) in [
                (&mut m.reel_name, &a.reel),
                (&mut m.scene, &a.scene),
                (&mut m.shot_take, &a.take),
                (&mut m.camera_angle, &a.angle),
            ] {
                if let Some(v) = v {
                    *field = Some(v.clone());
                }
            }
            e.save()?;
            println!("updated {}  ->  {}", e.asset.id, e.sidecar_path.display());
            count += 1;
        }
    }
    println!("{count} asset(s) updated");
    Ok(())
}

fn mark_cmd(a: MarkArgs) -> Result<()> {
    let rate = FrameRate::parse(&a.fps)?;
    let mut cat = Catalog::open(&a.root, false)?;
    let mut entries = cat.select_mut(&a.select, true)?;
    if entries.len() != 1 {
        return Err(AethrCraftError::Conflict(format!(
            "{:?} matches {} assets; be more specific",
            a.select,
            entries.len()
        )));
    }
    let e = &mut entries[0];
    if a.list {
        let r = e.xmp.markers.rate;
        for m in &e.xmp.markers.markers {
            println!(
                "{}  {}  {}{}  {}",
                format_timecode(m.start, r, false),
                format_timecode(m.end(), r, false),
                if m.subclip { "[subclip] " } else { "" },
                m.name,
                m.note
            );
        }
        return Ok(());
    }
    if !e.xmp.markers.markers.is_empty() && e.xmp.markers.rate != rate {
        return Err(AethrCraftError::Timecode(format!(
            "asset already has markers at {} fps; use --fps {}",
            e.xmp.markers.rate.label(),
            e.xmp.markers.rate.label()
        )));
    }
    e.xmp.markers.rate = rate;
    let (Some(name), Some(in_point)) = (a.name, a.in_point) else {
        return Err(AethrCraftError::SchemaError(
            "--name and --in are required".into(),
        ));
    };
    let (start, _) = parse_timecode(&in_point, rate)?;
    let duration = match &a.out_point {
        Some(o) => {
            let (end, _) = parse_timecode(o, rate)?;
            if end <= start {
                return Err(AethrCraftError::Timecode(
                    "Out point must be after In point".into(),
                ));
            }
            end - start
        }
        None => 0,
    };
    e.xmp.markers.add(Marker {
        name,
        note: a.note,
        start,
        duration,
        color: a.color,
        subclip: a.subclip,
    })?;
    e.save()?;
    println!(
        "{} marker(s) on {}",
        e.xmp.markers.markers.len(),
        e.asset.id
    );
    Ok(())
}

fn context_for(root: &Path, id: &str, profile: &UnifiedIngestProfile) -> Result<AssetContext> {
    let cat = Catalog::open(root, true)?;
    let e = cat
        .entries
        .iter()
        .find(|e| {
            e.asset.id == id
                || e.asset
                    .members
                    .iter()
                    .any(|m| aethrcraft::card::path_id(m) == id)
        })
        .ok_or_else(|| AethrCraftError::NotFound(id.to_string()))?;
    let m = &e.metadata;
    let t = &profile.iptc_template;
    Ok(AssetContext {
        original_filename: e
            .asset
            .primary
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        capture_timestamp: m.capture_time_utc().unwrap_or_else(chrono::Utc::now),
        camera_make: m.make.clone().unwrap_or_default(),
        camera_model: m.model.clone().unwrap_or_default(),
        camera_serial: m.serial.clone().unwrap_or_default(),
        iso: m.iso.unwrap_or(0),
        lens: m.lens.clone().unwrap_or_default(),
        shutter: m.shutter_string(),
        aperture: m.aperture_string(),
        focal_length: m.focal_string(),
        video_reel: e
            .xmp
            .meta
            .reel_name
            .clone()
            .or(t.reel_name.clone())
            .unwrap_or_default(),
        video_scene: e
            .xmp
            .meta
            .scene
            .clone()
            .or(t.scene.clone())
            .unwrap_or_default(),
        video_take: e
            .xmp
            .meta
            .shot_take
            .clone()
            .or(t.shot_take.clone())
            .unwrap_or_default(),
        camera_angle: e
            .xmp
            .meta
            .camera_angle
            .clone()
            .or(t.camera_angle.clone())
            .unwrap_or_default(),
        sequence_number: profile.sequence_start,
        card_name: cat.scan.card_name.clone(),
        ingest_timestamp: chrono::Utc::now(),
        extra: Default::default(),
    })
}

fn sample_context() -> AssetContext {
    use chrono::TimeZone;
    AssetContext {
        original_filename: "IMG_0001.CR3".into(),
        capture_timestamp: chrono::Utc
            .with_ymd_and_hms(2026, 11, 2, 18, 30, 0)
            .unwrap(),
        camera_make: "Canon".into(),
        camera_model: "EOS R5".into(),
        camera_serial: "012345678901".into(),
        iso: 800,
        lens: "RF24-70mm F2.8 L IS USM".into(),
        shutter: "1/1000".into(),
        aperture: "f2.8".into(),
        focal_length: "50mm".into(),
        video_reel: "A001".into(),
        video_scene: "12B".into(),
        video_take: "03".into(),
        camera_angle: "CAM_A".into(),
        sequence_number: 1,
        card_name: "CARD_01".into(),
        ingest_timestamp: chrono::Utc::now(),
        extra: Default::default(),
    }
}
