# AethrCraft

Clean-room media offload, culling and metadata engine in Rust. It combines the stills-culling speed of Photo Mechanic-style workflows with the verified video offload of Kyno-style workflows.

## Features

- **Legacy settings import:** reads Photo Mechanic `.pmx` XML with `.tsv` code replacements, and Kyno `.kynopreset` JSON. Both become one `UnifiedIngestProfile`, and the two can be merged into a single hybrid profile.
- **Variable engine:** expands unified `{tokens}` (`{date:YYYYMMDD}`, `{seq:4}`, `{camera:model}`, `{video:reel}`, …) and `\code\` / `\code#2\` replacements.
- **Single-pass ingest:** reads the source once, hashes it in flight (xxHash64, BLAKE3 or SHA-256), writes to N destinations and re-verifies every copy.
- **Zero-decode previews:** memory-maps RAW files and pulls out the largest embedded JPEG without demosaicing.

## Architecture

```text
            Legacy Settings Importer (.pmx / .tsv / .kynopreset)
                                 │ normalizes into
                                 ▼
   Unified Ingest & Culling Profile  [tokens] [code replacements] [checksum policy] [LUT]
                                 │ drives
                                 ▼
   Source card ──► 256 KB buffer ─┬─► streaming hasher (xxHash64 / BLAKE3 / SHA-256)
                                  ├─► writer → primary + backup destinations
                                  └─► post-write verification
```

| Module | Purpose |
| --- | --- |
| `profile` | `UnifiedIngestProfile`, `UnifiedMetadataTemplate`, `ChecksumAlgorithm` |
| `importers` | `SettingsImporter`: PM/Kyno parsing, token normalization, merge |
| `variables` | `VariableEngine`, `AssetContext` |
| `ingest` | `SinglePassIngestEngine`, `IngestReceipt` |
| `preview` | `ZeroDecodePreviewExtractor` |
| `error` | `AethrCraftError` |

## Roadmap

These parts of the design are not implemented yet: ASC-MHL manifest writing, XMP sidecar output, camera-card structure detection (`M4ROOT`, `BPAV`, `CONTENTS`, RED/ARRI), `.snap` import, 3D LUT application via `wgpu`, NLE export (FCPXML, Resolve ALE), and `io_uring` vectored I/O.

## Build

```bash
cargo test
```
