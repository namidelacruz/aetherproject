# AethrCraft

Clean-room media offload, culling and metadata engine in Rust. It combines the fast stills culling and captioning of Photo Mechanic-style tools with the verified, card-structure-aware video offload of Kyno-style tools. Ships as a library (`aethrcraft`) and a command-line tool (`aethrcraft`).

## Features

| Area | What you get |
| --- | --- |
| **Settings import** | Photo Mechanic `.pmx`, `.snap` IPTC snapshots and `.tsv`/`.txt` code replacements; Kyno `.kynopreset`; native TOML/JSON profiles (versioned, forward-compatible). Combine any mix with fixed precedence. |
| **Variables** | `{date:YYYYMMDD}`, `{date:HHmmss}`, `{seq}`, `{seq:4}`, `{camera:make\|model\|serial}`, `{exif:iso\|lens\|shutter\|aperture\|focal}`, `{video:reel\|scene\|take\|angle}`, `{file:stem\|ext\|name}`, `{card:name}`, `{meta:creator\|city\|…}`, `{ingest:…}`. PM and Kyno variables are translated token by token. |
| **Code replacements** | `\code\` and `\code#2\` (column select), loaded from PM tab-delimited files. |
| **Metadata reading** | No decoding needed. TIFF/EXIF RAW (NEF, ARW, DNG, CR2, ORF, RW2…), JPEG APP1, Fujifilm RAF, CR3/HEIC/MP4/MOV boxes, Sony `M01.XML` (device, fps, duration, start timecode). |
| **Card detection** | Sony XAVC `M4ROOT`, XDCAM EX `BPAV`, XDCAM `XDROOT`, Canon XF, Panasonic P2, AVCHD, RED `.RDC`, ARRIRAW/CinemaDNG sequences, DCIM, and generic folders. Each clip's files are grouped into one asset. |
| **Ingest** | Reads each source file once, writes to N destinations and computes several hashes in the same pass (xxHash64, BLAKE3, SHA-256, MD5, SHA-1). Every copy is re-read and verified, copied through temporary `.aethrpart` files, and keeps its original modification time. Files copy concurrently, sequence numbers carry over between runs, and there's a dry-run mode and a JSON report. |
| **ASC-MHL v2** | Each run adds a manifest "generation" to `ascmhl/`, linked by a C4-hashed `ascmhl_chain.xml`. `verify` re-hashes against the history, detects a tampered chain, and can record the result as a new generation. |
| **Culling** | Flat view of everything on a card, with filters (kind, rating, label, tag, text, extension). Ratings (-1 to 5), labels, tags, captions, keywords, reel/scene/take/angle. All stored in standard XMP sidecars. |
| **Markers & subclips** | Frame-accurate markers and In/Out subclips. SMPTE timecode, including drop-frame. |
| **Previews** | Largest embedded JPEG, found through file offsets first and then a scan (a thumbnail nested inside a preview can't cut it short). EXIF orientation, resizing, JPEG output. |
| **LUTs** | `.cube` files with 1D shaper + 3D, `DOMAIN_*` and `*_INPUT_RANGE` support. Trilinear interpolation, parallelised across image rows. Applied to previews and frames only, never to originals. |
| **Frame export** | Single video frames through `ffmpeg`, with an optional LUT. |
| **NLE interchange** | FCPXML 1.10 export/import (markers, subclips, keywords, ratings, reel/scene/take), ALE export/import, CSV export. |

## Conflict rules where features overlap

1. **Overlapping card structures.** A Sony card can hold both `DCIM` and `PRIVATE/M4ROOT`, and a RED `.RDC` sits inside an `.RDM`. Every file belongs to exactly one asset, and the deepest structure wins. Files inside a structure that belong to no clip (indexes, `MEDIAPRO.XML`) form a *support* asset, so preserving a structure never loses them.
2. **Renaming vs. structure lock.** Clips in camera structures are never renamed. With `preserve_camera_card_structure` the whole card is copied under `<folder>/<card name>/`. Without it, clip files keep their names, folder units (`.RDC`, sequences) stay intact, and support files are skipped with a warning.
3. **Date folders vs. structure integrity.** A preserved card's folder pattern is evaluated once, using the earliest capture time on that card, so a structure is never split across date folders.
4. **Paired files.** RAW+JPEG (+WAV, THM, LRV…) share one sequence number and one new name. Collision suffixes apply to the whole group.
5. **Name collisions.** Checked case-insensitively, both within a plan and against every destination. `Suffix` (default) renames renamable assets to `_N` and moves locked structures into a `<card>_N` folder. `Skip` re-hashes what already exists and reports a mismatch instead of overwriting. `Overwrite` replaces.
6. **Codes vs. tokens.** Codes are expanded first (so code text may contain `{tokens}`), then tokens are resolved once. Token *values* are never re-scanned, so a lens name containing `\` can't trigger a code. In paths, values are cleaned up (`/ \ : * ? " < > |`) and `..` is removed.
7. **Hash formats.** If the verification algorithm isn't an ASC-MHL format (BLAKE3, SHA-256), xxHash64 is computed in the same pass for the manifest.
8. **Metadata precedence.** Per-asset culling edits > older destination sidecar > profile template. Keywords are merged. Within settings: snapshot > PM stationery > Kyno metadata > base profile.
9. **Sidecar placement.** Never inside camera folders:
   - normal assets: `<stem>.xmp` beside the file;
   - folder units: `<dir>.xmp` beside the folder;
   - clips inside other structures: `_aethrcraft_sidecars/<path>.xmp` at the card root.

   The same rule applies at the source and the destination, so culling done on a card follows the files through ingest. Source XMP files are merged rather than copied, and are left out of the MHL.
10. **Unsafe setups are rejected.** Nested or overlapping destinations, a destination inside the source, folder patterns that escape the destination, and code delimiters that collide with token or path syntax are all errors.

## Command-line usage

```bash
cargo build --release
```

```bash
aethrcraft profile import --pmx ingest.pmx --codes roster.tsv --kyno eng.kynopreset -o studio.toml
```

```bash
aethrcraft scan /Volumes/CARD01
```

```bash
aethrcraft cull /Volumes/CARD01 DSC00001 --fuzzy --rating 5 --label Red --tag --caption '\p14\ scores' --profile studio.toml
```

```bash
aethrcraft mark /Volumes/CARD01 C0001.MP4 --name Goal --in 00:00:02:00 --out 00:00:05:00 --subclip
```

```bash
aethrcraft ingest /Volumes/CARD01 --profile studio.toml --dry-run
```

```bash
aethrcraft ingest /Volumes/CARD01 --profile studio.toml --report ingest.json --save-sequence
```

```bash
aethrcraft verify /Volumes/RAID
```

```bash
aethrcraft preview IMG_0001.CR3 -o preview.jpg --lut SLog3_to_709.cube --max-size 2048
```

```bash
aethrcraft frame C0001.MP4 --at 00:00:12:10 --fps 25 -o frame.jpg --lut look.cube
```

```bash
aethrcraft export /Volumes/RAID/2026-11-02/CARD01 --format fcpxml -o day1.fcpxml
```

```bash
aethrcraft import-nle /Volumes/RAID/2026-11-02/CARD01 edit.ale
```

```bash
aethrcraft expand '{date:YYYY}/{camera:model}/\ev1\' --profile studio.toml --path
```

Run `aethrcraft <command> --help` for every option.

## Library modules

| Module | Purpose |
| --- | --- |
| `profile` | `UnifiedIngestProfile`, validation and conflict rules, TOML/JSON |
| `importers` | `SettingsImporter`, `ImportSources` |
| `variables` | `VariableEngine`, `AssetContext`, template parser |
| `metadata` | `TechnicalMetadata`, EXIF/box/XML parsers |
| `card` | `CardScan`, `Asset`, `CardFormat` |
| `hashing` | `MultiHasher`, `hash_file` |
| `ingest` | `IngestPlanner`, `IngestExecutor`, `SinglePassIngestEngine` |
| `mhl` | ASC-MHL writer, chain, `verify` |
| `preview` | `ZeroDecodePreviewExtractor`, `PreviewRenderer`, frame export |
| `lut` | `CubeLut` |
| `markers` | `FrameRate`, timecode, `Marker`, `MarkerSet` |
| `xmp` | `XmpSidecar` |
| `catalog` | `Catalog`, `Filter` |
| `nle` | FCPXML / ALE / CSV |

## Implementation notes

- **CPU, not GPU, for LUTs.** LUTs run on CPU threads (rayon) instead of `wgpu` compute shaders. It's portable, testable without a GPU, and fast enough for previews.
- **Async I/O, not `io_uring`.** I/O goes through portable `tokio` async files. Each source file is still read only once. `io_uring` would make the tool Linux-only.
- **Frame export needs ffmpeg.** Exporting video frames requires `ffmpeg` on `PATH`. Everything else is pure Rust.
- **Legacy formats are clean-room guesses.** Photo Mechanic and Kyno file layouts are clean-room interpretations of their documented concepts. They import the fields listed in `importers.rs`.

## Tests

```bash
cargo test
```

23 tests cover settings import and precedence, token and code edge cases, path cleanup, metadata parsers, nested-thumbnail previews, LUTs, drop-frame timecode, XMP round-trips, detection of every camera format, ingest under both structure modes and all three collision policies, culling carried through ingest, MHL verification and tamper detection, and FCPXML/ALE round-trips.
