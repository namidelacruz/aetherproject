mod common;
use aethrcraft::catalog::Catalog;
use aethrcraft::ingest::CopyMode;
use aethrcraft::markers::FrameRate;
use aethrcraft::*;
use common::*;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const SONY_XML: &str = r#"<?xml version="1.0"?>
<NonRealTimeMeta><Duration value="500"/><CreationDate value="2026-11-02T18:30:00Z"/>
<VideoFormat><VideoFrame captureFps="25p"/></VideoFormat>
<Device manufacturer="Sony" modelName="ILME-FX6" serialNo="994812"/></NonRealTimeMeta>"#;

fn set_mtime(p: &Path, t: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// Builds a card with DCIM stills, a Sony XAVC structure, a RED clip and a note.
fn build_card(root: &Path) {
    let a1 = jpeg_with_exif(
        &jpeg(32, 32, [200, 10, 10]),
        &tiff_exif("Sony", "ILCE-1", 400, "2026:11:02 10:00:00", "FE 24-70"),
    );
    write(
        &root.join("DCIM/100MSDCF/DSC00001.ARW"),
        &tiff_exif("Sony", "ILCE-1", 400, "2026:11:02 10:00:00", "FE 24-70"),
    );
    write(&root.join("DCIM/100MSDCF/DSC00001.JPG"), &a1);
    write(
        &root.join("DCIM/100MSDCF/DSC00002.ARW"),
        &tiff_exif("Sony", "ILCE-1", 800, "2026:11:02 10:05:00", "FE 24-70"),
    );
    write(
        &root.join("PRIVATE/M4ROOT/CLIP/C0001.MP4"),
        &vec![1u8; 300_000],
    );
    write(
        &root.join("PRIVATE/M4ROOT/CLIP/C0001M01.XML"),
        SONY_XML.as_bytes(),
    );
    write(&root.join("PRIVATE/M4ROOT/SUB/C0001S03.MP4"), &[2u8; 1000]);
    write(
        &root.join("PRIVATE/M4ROOT/THMBNL/C0001T01.JPG"),
        &jpeg(16, 9, [0, 0, 255]),
    );
    write(
        &root.join("PRIVATE/M4ROOT/MEDIAPRO.XML"),
        b"<MediaProfile/>",
    );
    write(&root.join("PRIVATE/M4ROOT/STATUS.BIN"), &[0u8; 16]);
    let red = root.join("RED/A001.RDM/A001_C001.RDC");
    for f in ["A001_C001_001.R3D", "A001_C001_002.R3D", "A001_C001.RMD"] {
        write(&red.join(f), &[3u8; 2048]);
        set_mtime(
            &red.join(f),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_491_200),
        ); // 2026-11-01
    }
    write(&root.join("NOTES.txt"), b"shot notes");
    write(&root.join(".DS_Store"), b"junk");
}

fn profile(dests: Vec<PathBuf>) -> UnifiedIngestProfile {
    let mut p = UnifiedIngestProfile {
        destinations: dests,
        file_naming_pattern: "{date:YYYYMMDD}_{seq}".into(),
        ..Default::default()
    };
    p.iptc_template.creator = Some("Staff".into());
    p.iptc_template.caption_abstract = Some("{camera:model} frame {seq}".into());
    p
}

#[test]
fn detects_overlapping_structures_and_groups_files() {
    let dir = tempfile::tempdir().unwrap();
    let card = dir.path().join("card");
    build_card(&card);
    let scan = CardScan::scan(&card).unwrap();
    let formats: Vec<CardFormat> = scan.structures.iter().map(|s| s.format).collect();
    assert!(formats.contains(&CardFormat::Dcim));
    assert!(formats.contains(&CardFormat::SonyXavc));
    assert!(formats.contains(&CardFormat::Red));
    assert_eq!(
        scan.assets.len(),
        6,
        "{:#?}",
        scan.assets.iter().map(|a| &a.id).collect::<Vec<_>>()
    );

    let pair = scan.find("DCIM/100MSDCF/DSC00001.JPG").unwrap();
    assert_eq!(pair.primary, PathBuf::from("DCIM/100MSDCF/DSC00001.ARW"));
    assert_eq!(pair.members.len(), 2);
    assert!(!pair.locked);

    let clip = scan.find("PRIVATE/M4ROOT/CLIP/C0001.MP4").unwrap();
    assert!(clip.locked);
    assert_eq!(clip.members.len(), 4);
    assert_eq!(
        clip.sidecar_rel_path(),
        PathBuf::from("_aethrcraft_sidecars/PRIVATE/M4ROOT/CLIP/C0001.MP4.xmp")
    );

    let support = scan
        .assets
        .iter()
        .find(|a| a.kind == AssetKind::Support)
        .unwrap();
    assert_eq!(support.members.len(), 2);

    let red = scan.find("RED/A001.RDM/A001_C001.RDC").unwrap();
    assert_eq!(red.members.len(), 3);
    assert_eq!(
        red.sidecar_rel_path(),
        PathBuf::from("RED/A001.RDM/A001_C001.RDC.xmp")
    );
    assert!(scan.find(".DS_Store").is_none());
}

#[tokio::test]
async fn preserved_ingest_carries_culling_and_verifies_mhl() {
    let dir = tempfile::tempdir().unwrap();
    let card = dir.path().join("card");
    build_card(&card);
    // Cull on the source card first.
    {
        let mut cat = Catalog::open(&card, false).unwrap();
        for e in cat.select_mut("DSC00001", true).unwrap() {
            e.xmp.rating = Some(5);
            e.xmp.meta.keywords.push("hero".into());
            e.save().unwrap();
        }
        for e in cat
            .select_mut("PRIVATE/M4ROOT/CLIP/C0001.MP4", false)
            .unwrap()
        {
            e.xmp.label = Some("Green".into());
            e.save().unwrap();
        }
    }
    let (a, b) = (dir.path().join("raid"), dir.path().join("backup"));
    let prof = profile(vec![a.clone(), b.clone()]);
    let plan = IngestPlanner::plan(&card, &prof).unwrap();
    let report = IngestExecutor::execute(&plan, None).await.unwrap();
    assert!(report.ok(), "{:?}", report.errors);
    assert_eq!(report.mhl_manifests.len(), 2);

    for d in [&a, &b] {
        // Sequence follows capture time across all media (the RED clip from
        // 2026-11-01 is #1); stills are renamed as a pair.
        assert!(d.join("2026-11-02/20261102_0002.ARW").exists());
        assert!(d.join("2026-11-02/20261102_0002.JPG").exists());
        assert!(d.join("2026-11-02/20261102_0003.ARW").exists());
        // Whole card structure preserved once, under the earliest date.
        let base = d.join("2026-11-01/card");
        assert!(base.join("PRIVATE/M4ROOT/CLIP/C0001.MP4").exists());
        assert!(base.join("PRIVATE/M4ROOT/MEDIAPRO.XML").exists());
        assert!(base
            .join("RED/A001.RDM/A001_C001.RDC/A001_C001_002.R3D")
            .exists());
        assert!(!base.join("_aethrcraft_sidecars/../.DS_Store").exists());
        // No partial files left behind.
        assert!(walk(d)
            .iter()
            .all(|p| !p.to_string_lossy().ends_with(".aethrpart")));
    }

    // Sidecars: culling from the source + template expanded per asset.
    let x = XmpSidecar::load(&a.join("2026-11-02/20261102_0002.xmp"))
        .unwrap()
        .unwrap();
    assert_eq!(x.rating, Some(5));
    assert_eq!(x.meta.creator.as_deref(), Some("Staff"));
    assert_eq!(
        x.meta.caption_abstract.as_deref(),
        Some("ILCE-1 frame 0002")
    );
    assert!(x.meta.keywords.contains(&"hero".to_string()));
    assert_eq!(x.preserved_file_name.as_deref(), Some("DSC00001.ARW"));
    assert_eq!(x.checksum.as_ref().unwrap().0, "xxh64");

    // The destination is a valid catalog root with consistent sidecar paths.
    let dest_cat = Catalog::open(&a.join("2026-11-01/card"), true).unwrap();
    let clip = dest_cat
        .entries
        .iter()
        .find(|e| e.asset.id == "PRIVATE/M4ROOT/CLIP/C0001.MP4")
        .unwrap();
    assert_eq!(clip.xmp.label.as_deref(), Some("Green"));
    assert_eq!(clip.metadata.model.as_deref(), Some("ILME-FX6"));
    assert_eq!(clip.metadata.duration_seconds, Some(20.0));
    let red = dest_cat
        .entries
        .iter()
        .find(|e| e.asset.id.ends_with("A001_C001.RDC"))
        .unwrap();
    assert_eq!(red.xmp.meta.creator.as_deref(), Some("Staff"));

    // ASC-MHL verification, then tamper detection.
    let ok = mhl::verify(&a, true).await.unwrap();
    assert!(ok.ok() && ok.verified.len() >= 10, "{ok:?}");
    std::fs::write(a.join("2026-11-02/20261102_0003.ARW"), b"corrupt").unwrap();
    let bad = mhl::verify(&a, false).await.unwrap();
    assert_eq!(bad.failed.len(), 1);
    std::fs::remove_file(a.join("2026-11-01/card/PRIVATE/M4ROOT/STATUS.BIN")).unwrap();
    assert_eq!(mhl::verify(&a, false).await.unwrap().missing.len(), 1);
}

fn walk(d: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(d)
        .into_iter()
        .flatten()
        .map(|e| e.path().to_path_buf())
        .collect()
}

#[tokio::test]
async fn reingest_suffix_skip_and_overwrite_policies() {
    let dir = tempfile::tempdir().unwrap();
    let card = dir.path().join("card");
    build_card(&card);
    let dest = dir.path().join("raid");
    let mut prof = profile(vec![dest.clone()]);
    let r1 = IngestExecutor::execute(&IngestPlanner::plan(&card, &prof).unwrap(), None)
        .await
        .unwrap();
    assert!(r1.ok());

    // Suffix: locked structures get a new card folder, stills a stem suffix.
    let plan2 = IngestPlanner::plan(&card, &prof).unwrap();
    assert!(plan2.warnings.iter().any(|w| w.contains("card_2")));
    let r2 = IngestExecutor::execute(&plan2, None).await.unwrap();
    assert!(r2.ok(), "{:?}", r2.errors);
    assert!(dest
        .join("2026-11-01/card_2/PRIVATE/M4ROOT/CLIP/C0001.MP4")
        .exists());
    assert!(dest.join("2026-11-02/20261102_0002_1.ARW").exists());
    assert!(dest.join("2026-11-02/20261102_0002_1.JPG").exists());

    // Skip: everything already there is re-hashed instead of copied.
    prof.collision_policy = CollisionPolicy::Skip;
    let plan3 = IngestPlanner::plan(&card, &prof).unwrap();
    assert!(plan3
        .assets
        .iter()
        .flat_map(|a| &a.files)
        .all(|f| f.mode == CopyMode::KeepExisting));
    let r3 = IngestExecutor::execute(&plan3, None).await.unwrap();
    assert!(r3.ok(), "{:?}", r3.errors);
    assert_eq!(r3.files_copied, 0);

    // Skip with different content is reported, never overwritten.
    std::fs::write(dest.join("2026-11-02/20261102_0003.ARW"), b"different").unwrap();
    let r4 = IngestExecutor::execute(&IngestPlanner::plan(&card, &prof).unwrap(), None)
        .await
        .unwrap();
    assert_eq!(r4.errors.len(), 1);
    assert_eq!(
        std::fs::read(dest.join("2026-11-02/20261102_0003.ARW")).unwrap(),
        b"different"
    );

    // Overwrite replaces it.
    prof.collision_policy = CollisionPolicy::Overwrite;
    let r5 = IngestExecutor::execute(&IngestPlanner::plan(&card, &prof).unwrap(), None)
        .await
        .unwrap();
    assert!(r5.ok());
    assert_ne!(
        std::fs::read(dest.join("2026-11-02/20261102_0003.ARW")).unwrap(),
        b"different"
    );
}

#[tokio::test]
async fn flattened_ingest_keeps_locked_names_and_skips_support() {
    let dir = tempfile::tempdir().unwrap();
    let card = dir.path().join("card");
    build_card(&card);
    let dest = dir.path().join("out");
    let mut prof = profile(vec![dest.clone()]);
    prof.preserve_camera_card_structure = false;
    // No sequence number in the name: plan-level collisions must be resolved.
    prof.file_naming_pattern = "{date:YYYYMMDD}".into();
    prof.verify_checksum = ChecksumAlgorithm::Sha256;
    let plan = IngestPlanner::plan(&card, &prof).unwrap();
    assert_eq!(plan.skipped_support_files, 2);
    let r = IngestExecutor::execute(&plan, None).await.unwrap();
    assert!(r.ok(), "{:?}", r.errors);
    let day = dest.join("2026-11-02");
    assert!(day.join("20261102.ARW").exists() && day.join("20261102.JPG").exists());
    assert!(day.join("20261102_1.ARW").exists());
    for f in [
        "C0001.MP4",
        "C0001M01.XML",
        "C0001S03.MP4",
        "C0001T01.JPG",
        "C0001.xmp",
    ] {
        assert!(day.join(f).exists(), "{f}");
    }
    assert!(!dest.join("2026-11-02/MEDIAPRO.XML").exists());
    let red = dest.join("2026-11-01");
    assert!(red.join("A001_C001.RDC/A001_C001_001.R3D").exists());
    assert!(red.join("A001_C001.RDC.xmp").exists());
    // MHL used xxh64 alongside SHA-256 verification.
    assert!(mhl::verify(&dest, false).await.unwrap().ok());
    let mhl_text = std::fs::read_to_string(
        walk(&dest.join("ascmhl"))
            .into_iter()
            .find(|p| p.extension().is_some_and(|e| e == "mhl"))
            .unwrap(),
    )
    .unwrap();
    assert!(mhl_text.contains("<xxh64 action=\"original\""));
}

#[test]
fn rejects_destination_inside_source() {
    let dir = tempfile::tempdir().unwrap();
    let card = dir.path().join("card");
    build_card(&card);
    let prof = profile(vec![card.join("backup")]);
    assert!(matches!(
        IngestPlanner::plan(&card, &prof),
        Err(AethrCraftError::Conflict(_))
    ));
}

#[test]
fn nle_round_trips_through_ale_and_fcpxml() {
    let dir = tempfile::tempdir().unwrap();
    let card = dir.path().join("card");
    build_card(&card);
    let rate = FrameRate::parse("25").unwrap();
    {
        let mut cat = Catalog::open(&card, true).unwrap();
        let e = cat.select_mut("C0001.MP4", true).unwrap().pop().unwrap();
        e.xmp.markers.rate = rate;
        e.xmp
            .markers
            .add(Marker {
                name: "Goal".into(),
                note: "n".into(),
                start: 50,
                duration: 0,
                color: None,
                subclip: false,
            })
            .unwrap();
        e.xmp
            .markers
            .add(Marker {
                name: "Replay".into(),
                note: String::new(),
                start: 100,
                duration: 75,
                color: None,
                subclip: true,
            })
            .unwrap();
        e.xmp.meta.scene = Some("12B".into());
        e.xmp.meta.keywords = vec!["goal".into()];
        e.xmp.rating = Some(5);
        e.save().unwrap();
    }
    let cat = Catalog::open(&card, true).unwrap();
    let media: Vec<_> = cat.entries.iter().filter(|e| e.asset.is_media()).collect();

    let ale = nle::export_ale(&cat, &media, rate);
    assert!(ale.contains("C0001.MP4\t\t00:00:00:00\t00:00:20:00"));
    let fcp = nle::export_fcpxml(&cat, &media, "Day 1", rate);
    assert!(fcp.contains("<marker start=\"2s\"") && fcp.contains("value=\"favorite\""));

    // Re-import into a fresh copy of the card with no sidecars.
    let copy = dir.path().join("copy");
    build_card(&copy);
    let mut cat2 = Catalog::open(&copy, false).unwrap();
    let clips = nle::parse_fcpxml(&fcp, rate);
    let clip = clips.iter().find(|c| c.name == "C0001.MP4").unwrap();
    assert_eq!(clip.markers.len(), 2);
    // Source paths point at the original card; match falls back to names.
    assert!(nle::apply_import(&mut cat2, &clips, rate).unwrap() >= 1);
    let ale_clips = nle::parse_ale(&ale);
    nle::apply_import(&mut cat2, &ale_clips, rate).unwrap();

    let reloaded = Catalog::open(&copy, false).unwrap();
    let e = reloaded
        .entries
        .iter()
        .find(|e| e.asset.id.ends_with("C0001.MP4"))
        .unwrap();
    assert_eq!(e.xmp.markers.markers.len(), 2);
    assert!(e
        .xmp
        .markers
        .subclips()
        .any(|m| m.name == "Replay" && m.duration == 75));
    assert_eq!(e.xmp.meta.scene.as_deref(), Some("12B"));
    assert_eq!(e.xmp.rating, Some(5));
    assert_eq!(e.xmp.meta.keywords, vec!["goal"]);

    let csv = nle::export_csv(&media);
    assert!(csv.lines().count() == media.len() + 1);
}

#[tokio::test]
async fn single_pass_engine_all_algorithms() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let payload: Vec<u8> = (0..(600 * 1024u32)).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &payload).unwrap();
    let dests = vec![dir.path().join("a/x.bin"), dir.path().join("b/x.bin")];
    for algo in ChecksumAlgorithm::ALL
        .into_iter()
        .chain([ChecksumAlgorithm::None])
    {
        let r = SinglePassIngestEngine::offload_asset(&src, &dests, algo)
            .await
            .unwrap();
        assert_eq!(r.bytes_copied, payload.len() as u64);
        for d in &dests {
            assert_eq!(std::fs::read(d).unwrap(), payload);
        }
        if algo != ChecksumAlgorithm::None {
            assert_eq!(r.digest_hex, hashing::hash_bytes(&payload, algo));
        }
    }
    assert_eq!(
        hashing::hash_bytes(b"abc", ChecksumAlgorithm::Md5),
        "900150983cd24fb0d6963f7d28e17f72"
    );
    assert_eq!(
        hashing::hash_bytes(b"abc", ChecksumAlgorithm::Sha1),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(mhl::c4_id(b"").len(), 90);
}

#[test]
fn detects_other_camera_formats() {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    // Panasonic P2: clip 0001AB spans VIDEO, AUDIO, CLIP, ICON.
    for f in [
        "CONTENTS/VIDEO/0001AB.MXF",
        "CONTENTS/AUDIO/0001AB00.MXF",
        "CONTENTS/AUDIO/0001AB01.MXF",
        "CONTENTS/CLIP/0001AB.XML",
        "CONTENTS/ICON/0001AB.BMP",
        "CONTENTS/VIDEO/0002CD.MXF",
        "LASTCLIP.TXT",
    ] {
        write(&r.join("p2").join(f), b"x");
    }
    // Canon XF.
    for f in [
        "CONTENTS/CLIPS001/AA0001/AA000101.MXF",
        "CONTENTS/CLIPS001/AA0001/AA0001.XML",
        "CONTENTS/CLIPS001/AA0001/AA0001.CIF",
        "CONTENTS/CLIPS001/INDEX.MIF",
    ] {
        write(&r.join("xf").join(f), b"x");
    }
    // XDCAM EX.
    for f in [
        "BPAV/CLPR/791_0001_01/791_0001_01.MP4",
        "BPAV/CLPR/791_0001_01/791_0001_01M01.XML",
        "BPAV/MEDIAPRO.XML",
    ] {
        write(&r.join("ex").join(f), b"x");
    }
    // AVCHD.
    for f in [
        "PRIVATE/AVCHD/BDMV/STREAM/00000.MTS",
        "PRIVATE/AVCHD/BDMV/CLIPINF/00000.CPI",
        "PRIVATE/AVCHD/BDMV/INDEX.BDM",
    ] {
        write(&r.join("avchd").join(f), b"x");
    }
    // ARRIRAW frame sequence.
    for i in 0..3 {
        write(&r.join(format!("arri/A001C001/A001C001.{i:07}.ari")), b"x");
    }

    let p2 = CardScan::scan(&r.join("p2")).unwrap();
    assert_eq!(p2.structures[0].format, CardFormat::PanasonicP2);
    let clip = p2.find("CONTENTS/VIDEO/0001AB.MXF").unwrap();
    assert_eq!(clip.members.len(), 5);
    assert!(p2.find("CONTENTS/VIDEO/0002CD.MXF").is_some());
    assert!(p2
        .find("LASTCLIP.TXT")
        .is_some_and(|a| a.kind == AssetKind::Other));

    let xf = CardScan::scan(&r.join("xf")).unwrap();
    assert_eq!(xf.structures[0].format, CardFormat::CanonXf);
    assert_eq!(
        xf.find("CONTENTS/CLIPS001/AA0001/AA000101.MXF")
            .unwrap()
            .members
            .len(),
        3
    );
    assert!(xf
        .assets
        .iter()
        .any(|a| a.kind == AssetKind::Support && a.members.len() == 1));

    let ex = CardScan::scan(&r.join("ex")).unwrap();
    assert_eq!(ex.structures[0].format, CardFormat::SonyXdcamEx);
    assert_eq!(
        ex.find("BPAV/CLPR/791_0001_01/791_0001_01.MP4")
            .unwrap()
            .members
            .len(),
        2
    );

    let av = CardScan::scan(&r.join("avchd")).unwrap();
    assert_eq!(av.structures[0].format, CardFormat::Avchd);
    assert_eq!(av.structures[0].root, PathBuf::from("PRIVATE/AVCHD"));
    assert_eq!(
        av.find("PRIVATE/AVCHD/BDMV/STREAM/00000.MTS")
            .unwrap()
            .members
            .len(),
        2
    );

    let arri = CardScan::scan(&r.join("arri")).unwrap();
    assert_eq!(arri.structures[0].format, CardFormat::FrameSequence);
    let seq = arri.find("A001C001").unwrap();
    assert_eq!(seq.members.len(), 3);
    assert_eq!(seq.kind, AssetKind::Video);
}
