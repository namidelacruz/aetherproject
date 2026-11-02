use aethrcraft::importers::ImportSources;
use aethrcraft::*;
use chrono::{TimeZone, Utc};
use std::io::Write;

const PMX_SAMPLE: &str = r#"
<PhotoMechanic>
    <IngestSettings>
        <primary_destination>/mnt/raid_primary</primary_destination>
        <secondary_destination>/mnt/nvme_scratch</secondary_destination>
        <folder_name>{datesort}_{model}</folder_name>
        <rename_string>{datesort}_{seqn}_{filenamebase}</rename_string>
        <sequence_num>101</sequence_num>
    </IngestSettings>
    <IPTCStationery>
        <photographer>Staff Photographer</photographer>
        <credit>Press Pool</credit>
        <copyright>(c) {year4} Press Pool</copyright>
        <caption>\ev1\ - \p14\ in action (\p14#2\) during Reel {video:reel}.</caption>
        <headline>\ev1#2\</headline>
        <city>Calgary</city>
        <state>AB</state>
        <country>Canada</country>
        <keywords>sports, broadcast, live</keywords>
    </IPTCStationery>
</PhotoMechanic>
"#;

const KYNO_SAMPLE: &str = r#"{
    "name": "Broadcast ENG Offload",
    "targetPath": "/mnt/raid_primary",
    "backupPaths": ["/mnt/lto_staging"],
    "namingPattern": "$clip.creationDate_$clip.reel_$index",
    "subfolderPattern": "${clip.year}/${clip.month}",
    "verifyMode": "BLAKE3",
    "writeMhl": true,
    "keepCardStructure": true,
    "lutFile": "/luts/Sony_SLog3_to_Rec709.cube",
    "metadata": { "reel": "A001", "scene": "12B", "take": "03", "camera": "CAM_A", "tags": ["4K", "SLog3"] }
}"#;

fn ctx(seq: u64) -> AssetContext {
    AssetContext {
        original_filename: "C0042.MP4".to_string(),
        capture_timestamp: Utc.with_ymd_and_hms(2026, 11, 2, 18, 30, 5).unwrap(),
        camera_model: "FX6".to_string(),
        camera_serial: "SN994812".to_string(),
        iso: 800,
        lens: "24-70mm F2.8 GM II".to_string(),
        video_reel: "A001".to_string(),
        video_scene: "12B".to_string(),
        video_take: "03".to_string(),
        sequence_number: seq,
        ..Default::default()
    }
}

fn codes_file(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("pm_codes.tsv");
    let mut f = std::fs::File::create(&p).unwrap();
    writeln!(f, "# comment line").unwrap();
    writeln!(f, "ev1\tFinals Game 4\tChampionship Series").unwrap();
    writeln!(f, "p14\tAlex Mercer\tForward #14").unwrap();
    writeln!(f, "yr\t{{date:YYYY}} season").unwrap();
    p
}

#[test]
fn hybrid_pm_and_kyno_import_and_evaluation() {
    let dir = tempfile::tempdir().unwrap();
    let tsv = codes_file(dir.path());
    let pm = SettingsImporter::from_photo_mechanic(PMX_SAMPLE, Some(&tsv)).unwrap();
    assert_eq!(pm.sequence_start, 101);
    assert_eq!(
        pm.iptc_template.keywords,
        vec!["sports", "broadcast", "live"]
    );

    let kyno = SettingsImporter::from_kyno_preset(KYNO_SAMPLE).unwrap();
    assert_eq!(
        kyno.file_naming_pattern,
        "{date:YYYYMMDD}_{video:reel}_{seq:4}"
    );
    assert_eq!(kyno.folder_naming_pattern, "{date:YYYY}/{date:MM}");

    let hybrid = SettingsImporter::merge_pm_and_kyno(pm, kyno);
    assert_eq!(hybrid.destinations.len(), 3);
    assert_eq!(hybrid.verify_checksum, ChecksumAlgorithm::Blake3);
    assert!(hybrid.generate_asc_mhl);
    assert!(hybrid.preserve_camera_card_structure);
    assert_eq!(hybrid.iptc_template.keywords.len(), 5);
    assert_eq!(hybrid.iptc_template.reel_name.as_deref(), Some("A001"));

    let c = ctx(hybrid.sequence_start);
    let t = &hybrid.iptc_template;
    assert_eq!(
        VariableEngine::evaluate(t.caption_abstract.as_ref().unwrap(), &c, &hybrid),
        "Finals Game 4 - Alex Mercer in action (Forward #14) during Reel A001."
    );
    assert_eq!(
        VariableEngine::evaluate(t.headline.as_ref().unwrap(), &c, &hybrid),
        "Championship Series"
    );
    assert_eq!(
        VariableEngine::evaluate(t.copyright_notice.as_ref().unwrap(), &c, &hybrid),
        "(c) 2026 Press Pool"
    );
    assert_eq!(
        VariableEngine::evaluate_file_name(&hybrid.file_naming_pattern, &c, &hybrid),
        "20261102_0101_C0042"
    );
    assert_eq!(
        VariableEngine::evaluate_path(&hybrid.folder_naming_pattern, &c, &hybrid),
        "20261102_FX6"
    );
}

#[test]
fn codes_expand_before_tokens_and_values_are_not_rescanned() {
    let dir = tempfile::tempdir().unwrap();
    let p = UnifiedIngestProfile {
        code_replacements: SettingsImporter::parse_pm_code_replacements(&codes_file(dir.path()))
            .unwrap(),
        ..Default::default()
    };
    let mut c = ctx(1);
    // A metadata value containing the delimiter must not trigger expansion.
    c.lens = r"\p14\".into();
    assert_eq!(
        VariableEngine::evaluate(r"\yr\ {exif:lens}", &c, &p),
        r"2026 season \p14\"
    );
    // Unknown codes stay intact; scanning resumes at the closing delimiter.
    assert_eq!(
        VariableEngine::evaluate(r"C:\Users\p14\x", &c, &p),
        r"C:\UsersAlex Mercerx"
    );
    assert_eq!(VariableEngine::evaluate(r"\nobody\", &c, &p), r"\nobody\");
    // Out-of-range column falls back to column 1.
    assert_eq!(VariableEngine::evaluate(r"\p14#9\", &c, &p), "Alex Mercer");
    // Unknown tokens are kept and reported.
    assert_eq!(VariableEngine::evaluate("{bogus:x}", &c, &p), "{bogus:x}");
    assert_eq!(
        VariableEngine::unknown_tokens("{seq} {bogus:x}"),
        vec!["{bogus:x}"]
    );
}

#[test]
fn path_mode_sanitizes_values_and_blocks_traversal() {
    let p = UnifiedIngestProfile::default();
    let mut c = ctx(7);
    c.camera_model = "EOS R5/R6: \"Mk II\"".into();
    c.video_reel = "..".into();
    let out = VariableEngine::evaluate_path("{date:YYYY}/{camera:model}/{video:reel}/x", &c, &p);
    assert_eq!(out, "2026/EOS R5_R6_ _Mk II_/x");
    assert_eq!(
        VariableEngine::evaluate_path(r"{date:YYYY}\{date:MM}", &c, &p),
        "2026/11"
    );
    assert_eq!(
        VariableEngine::evaluate_file_name("{date:HHmmss}_{seq:3}", &c, &p),
        "183005_007"
    );
}

#[test]
fn token_translation_is_token_level() {
    assert_eq!(
        SettingsImporter::normalize_pm_tokens("{datesort}-{model}-{unknown}-{camera:model}"),
        "{date:YYYYMMDD}-{camera:model}-{unknown}-{camera:model}"
    );
    assert_eq!(
        SettingsImporter::normalize_kyno_tokens("$clip.cameraModel_$clip.camera_${index:4}_$foo"),
        "{camera:model}_{video:angle}_{seq:4}_$foo"
    );
}

#[test]
fn snapshot_import_and_combine_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let snap = dir.path().join("event.snap");
    std::fs::write(&snap, "<IPTCStationery><photographer>Snap Shooter</photographer><city>Edmonton</city><keywords>hockey; playoffs</keywords></IPTCStationery>").unwrap();
    let bare = SettingsImporter::from_photo_mechanic_snapshot(
        "<Snapshot><headline>Bare</headline></Snapshot>",
    )
    .unwrap();
    assert_eq!(bare.headline.as_deref(), Some("Bare"));

    let pmx = dir.path().join("a.pmx");
    std::fs::write(&pmx, PMX_SAMPLE).unwrap();
    let kyno = dir.path().join("b.kynopreset");
    std::fs::write(&kyno, KYNO_SAMPLE).unwrap();
    let combined = SettingsImporter::combine(&ImportSources {
        pmx: Some(pmx),
        kyno: Some(kyno),
        snapshot: Some(snap),
        codes: Some(codes_file(dir.path())),
        native: None,
    })
    .unwrap();
    // Snapshot overrides PM stationery, which overrides Kyno.
    assert_eq!(
        combined.iptc_template.creator.as_deref(),
        Some("Snap Shooter")
    );
    assert_eq!(combined.iptc_template.city.as_deref(), Some("Edmonton"));
    assert_eq!(
        combined.iptc_template.credit_line.as_deref(),
        Some("Press Pool")
    );
    assert!(combined
        .iptc_template
        .keywords
        .contains(&"playoffs".to_string()));
    assert!(combined
        .iptc_template
        .keywords
        .contains(&"SLog3".to_string()));
    assert_eq!(combined.verify_checksum, ChecksumAlgorithm::Blake3);
    assert_eq!(combined.code_replacements.len(), 3);
}

#[test]
fn profile_validation_resolves_conflicts() {
    let mut p = UnifiedIngestProfile {
        destinations: vec!["/a/b".into(), "/a/b/".into(), "/c".into()],
        verify_checksum: ChecksumAlgorithm::Blake3,
        ..Default::default()
    };
    let w = p.validate().unwrap();
    assert_eq!(p.destinations.len(), 2);
    assert!(w.iter().any(|w| w.contains("duplicate")));
    assert!(w.iter().any(|w| w.contains("ASC-MHL")));
    assert_eq!(p.mhl_algorithm(), Some(ChecksumAlgorithm::XxHash64));
    assert_eq!(
        p.hash_algorithms(),
        vec![ChecksumAlgorithm::Blake3, ChecksumAlgorithm::XxHash64]
    );

    let mut nested = UnifiedIngestProfile {
        destinations: vec!["/a".into(), "/a/b".into()],
        ..Default::default()
    };
    assert!(nested.validate().is_err());
    let mut bad_delim = UnifiedIngestProfile {
        code_replacement_delimiter: '{',
        ..Default::default()
    };
    assert!(bad_delim.validate().is_err());
    let mut escape = UnifiedIngestProfile {
        folder_naming_pattern: "../x".into(),
        ..Default::default()
    };
    assert!(escape.validate().is_err());
}

#[test]
fn profile_round_trips_through_toml_and_json() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = UnifiedIngestProfile::default();
    p.code_replacements
        .insert("k".into(), vec!["v1".into(), "v2".into()]);
    p.iptc_template.keywords = vec!["a".into()];
    for name in ["p.toml", "p.json"] {
        let path = dir.path().join(name);
        p.save(&path).unwrap();
        let back = UnifiedIngestProfile::load(&path).unwrap();
        assert_eq!(back.code_replacements, p.code_replacements);
        assert_eq!(back.iptc_template, p.iptc_template);
    }
    // Missing fields fall back to defaults (forward-compatible loading).
    let partial = dir.path().join("partial.toml");
    std::fs::write(&partial, "profile_name = \"x\"\n").unwrap();
    let loaded = UnifiedIngestProfile::load(&partial).unwrap();
    assert_eq!(loaded.profile_name, "x");
    assert!(loaded.generate_asc_mhl);
}
