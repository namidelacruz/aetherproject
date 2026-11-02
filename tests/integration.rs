use aethrcraft::*;
use chrono::{TimeZone, Utc};
use std::fs::File;
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
    "metadata": {
        "reel": "A001",
        "scene": "12B",
        "take": "03",
        "camera": "CAM_A",
        "tags": ["4K", "SLog3"]
    }
}"#;

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aethrcraft_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample_ctx(seq: u64) -> AssetContext {
    AssetContext {
        original_filename: "C0042.MP4".to_string(),
        capture_timestamp: Utc.with_ymd_and_hms(2026, 11, 2, 18, 30, 0).unwrap(),
        camera_model: "FX6".to_string(),
        camera_serial: "SN994812".to_string(),
        iso: 800,
        lens: "24-70mm F2.8 GM II".to_string(),
        video_reel: "A001".to_string(),
        video_scene: "12B".to_string(),
        video_take: "03".to_string(),
        sequence_number: seq,
    }
}

#[test]
fn hybrid_pm_and_kyno_import_and_evaluation() {
    let dir = scratch_dir("hybrid");
    let tsv_path = dir.join("pm_codes.tsv");
    {
        let mut f = File::create(&tsv_path).unwrap();
        writeln!(f, "# comment line").unwrap();
        writeln!(f, "ev1\tFinals Game 4\tChampionship Series").unwrap();
        writeln!(f, "p14\tAlex Mercer\tForward #14").unwrap();
    }

    let pm = SettingsImporter::from_photo_mechanic(PMX_SAMPLE, Some(&tsv_path)).unwrap();
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

    let ctx = sample_ctx(hybrid.sequence_start);

    let caption = hybrid.iptc_template.caption_abstract.as_ref().unwrap();
    assert_eq!(
        VariableEngine::evaluate(caption, &ctx, &hybrid),
        "Finals Game 4 - Alex Mercer in action (Forward #14) during Reel A001."
    );

    let headline = hybrid.iptc_template.headline.as_ref().unwrap();
    assert_eq!(
        VariableEngine::evaluate(headline, &ctx, &hybrid),
        "Championship Series"
    );

    let copyright = hybrid.iptc_template.copyright_notice.as_ref().unwrap();
    assert_eq!(
        VariableEngine::evaluate(copyright, &ctx, &hybrid),
        "(c) 2026 Press Pool"
    );

    assert_eq!(
        VariableEngine::evaluate(&hybrid.file_naming_pattern, &ctx, &hybrid),
        "20261102_0101_C0042"
    );
    assert_eq!(
        VariableEngine::evaluate(&hybrid.folder_naming_pattern, &ctx, &hybrid),
        "20261102_FX6"
    );
}

#[test]
fn unknown_code_replacement_is_left_intact() {
    let profile = SettingsImporter::from_kyno_preset(KYNO_SAMPLE).unwrap();
    let out = VariableEngine::evaluate(r"Shot by \nobody\", &sample_ctx(1), &profile);
    assert_eq!(out, r"Shot by \nobody\");
}

#[tokio::test]
async fn single_pass_offload_writes_and_verifies_all_destinations() {
    let dir = scratch_dir("offload");
    let src = dir.join("card/DCIM/100MSDCF/DSC0001.ARW");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    // > 1 chunk so the streaming loop iterates multiple times.
    let payload: Vec<u8> = (0..(600 * 1024u32)).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &payload).unwrap();

    let dests = vec![
        dir.join("primary/a/DSC0001.ARW"),
        dir.join("backup/b/DSC0001.ARW"),
    ];

    for algo in [
        ChecksumAlgorithm::XxHash64,
        ChecksumAlgorithm::Sha256,
        ChecksumAlgorithm::Blake3,
        ChecksumAlgorithm::None,
    ] {
        let receipt = SinglePassIngestEngine::offload_asset(&src, &dests, algo)
            .await
            .unwrap();
        assert_eq!(receipt.bytes_copied, payload.len() as u64);
        for d in &dests {
            assert_eq!(std::fs::read(d).unwrap(), payload);
        }
        if algo == ChecksumAlgorithm::Blake3 {
            assert_eq!(
                receipt.digest_hex,
                blake3::hash(&payload).to_hex().to_string()
            );
        }
        if algo == ChecksumAlgorithm::None {
            assert_eq!(receipt.digest_hex, "UNVERIFIED");
        }
    }
}

#[test]
fn zero_decode_preview_picks_largest_jpeg() {
    let dir = scratch_dir("preview");
    let raw = dir.join("IMG_0001.CR3");

    let small = [0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 0xFF, 0xD9];
    let mut large = vec![0xFF, 0xD8, 0xFF, 0xDB];
    large.extend(std::iter::repeat_n(0x42, 1000));
    large.extend([0xFF, 0xD9]);

    let mut bytes = vec![0u8; 64];
    bytes.extend(&small);
    bytes.extend(vec![0x11; 128]);
    bytes.extend(&large);
    bytes.extend([0xFF, 0xD8, 0xFF, 0x00]); // truncated trailing SOI
    std::fs::write(&raw, &bytes).unwrap();

    let found = ZeroDecodePreviewExtractor::find_largest_embedded_jpeg(&raw).unwrap();
    assert_eq!(found.as_deref(), Some(large.as_slice()));

    let empty = dir.join("empty.NEF");
    std::fs::write(&empty, b"").unwrap();
    assert!(
        ZeroDecodePreviewExtractor::find_largest_embedded_jpeg(&empty)
            .unwrap()
            .is_none()
    );
}
