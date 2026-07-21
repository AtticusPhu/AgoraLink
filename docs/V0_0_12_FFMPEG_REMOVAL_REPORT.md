# AgoraLink v0.0.12 FFmpeg Removal Report

Date: 2026-07-21

Source commit: `ba4369dfffc0d3efdba622050586f39c0c16404d`

## Result

`PASS`

AgoraLink v0.0.12 production behavior is native-only. The application no longer offers, discovers, invokes, packages, or recommends FFmpeg, ffprobe, or ffplay.

## Runtime and UI

- Removed external backend selection, executable paths, install/download/select controls, and external capability/profile diagnostics.
- Removed external command construction, process readers, stderr parsers, stop branches, capability caches, executable discovery, and PATH lookup.
- Screen sharing uses the bundled Rust native runtime. A missing native executable is reported as a screen-sharing capability error without blocking chat or file transfer.
- Native system audio capability is reported by the native runtime; no FFmpeg fallback is presented.

## Configuration Migration

`config_migration.py` retains the strings `ffmpeg` and `external` solely to migrate old user settings. Migration writes `backend=native`, removes obsolete source/path/profile keys, emits `legacy_ffmpeg_config_migrated_to_native` once, and is idempotent.

Intentional remaining references are limited to migration, tests, historical documentation, and forbidden-artifact scanners. They are not runtime entry points.

## Packaging

- `AgoraLink.spec` bundles one `_internal/tools/agoralink_media/agoralink_media.exe` and no external media tools.
- Optional Kivy video, ffpyplayer, GStreamer, and gstplayer providers are excluded from analysis/data and rejected by final scanning.
- The v0.0.12 packager neither injects an external tools directory nor changes PATH for one.
- Staging and independently extracted trees are scanned using the same fail-closed rules.

## Final Portable Evidence

Candidate:

`_local_artifacts/V0_0_12_RELEASE_FINAL_ba4369d/AgoraLink_v0.0.12_portable.zip`

- Size: 41,881,098 bytes.
- SHA-256: `9927AE652064670E9CE771B4BA93D0F13B38BA33DDCE697E60F7ECFAB4CCF479`.
- ZIP entries: 1,447.
- Files scanned in staging and extraction: 1,434 each.
- `ffmpeg.exe`, `ffprobe.exe`, `ffplay.exe`: 0.
- FFmpeg-family named libraries: 0.
- ffpyplayer/GStreamer/gstplayer provider files: 0.
- External media bundle directories: 0.
- PDB, DMP, and source files: 0.
- Staging scan: PASS.
- Extracted scan: PASS.

## Process Evidence

The full deterministic run and independent GUI launch ended with zero residual `ffmpeg`, `ffprobe`, `ffplay`, and `agoralink_media` processes. Python command-builder tests verify that only native `screen-send` and `screen-recv` commands are generated.

Real WGC/QSV/D3D11 and dual-host sessions were not executed in this task, so this report makes no hardware or LAN quality claim.

## Preserved Native Pipeline

- Sender: WGC -> D3D11 conversion -> QSV/WMF H.264 -> AGM1 UDP.
- Receiver: AGM1 UDP -> reassembly/NACK -> WMF H.264 -> D3D11 render.
- Audio: WASAPI/native UDP when native capability is available.
