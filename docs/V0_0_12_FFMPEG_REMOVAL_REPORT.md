# AgoraLink v0.0.12 FFmpeg Removal Report

Date: 2026-07-21

Decision: all AgoraLink media behavior uses the bundled native Windows runtime.

## Result

`PASS`

The application no longer offers, discovers, invokes, packages, or recommends FFmpeg, ffprobe, or ffplay. The removal covers production Python, UI, settings, diagnostics, process management, PyInstaller data, current packaging scripts, installer messaging, dependencies, and current README guidance.

## Runtime and UI

- Removed backend selection when only the native backend remains.
- Removed external executable paths, capability profiles, codec/hardware probe UI, install/download/select actions, and external process status.
- Removed external command construction, process readers, stderr parsing, stop branches, capability caches, executable discovery, and PATH lookup.
- Native screen-sharing unavailability now produces a native capability error rather than installation guidance.

## Configuration Migration

`config_migration.py` recognizes the old `ffmpeg` and `external` backend aliases only for one-time migration. It writes `backend=native`, removes legacy executable/source/profile keys, emits `legacy_ffmpeg_config_migrated_to_native` once, and is idempotent after persistence.

These migration literals are the only intentional production-code references to the removed backend names. The portable privacy scanner also contains those names as a rejection pattern. Tests and historical audit/changelog documents retain references as evidence.

## Packaging

- `AgoraLink.spec` requires and bundles only `agoralink_media.exe` for screen media.
- Optional Kivy file-video, ffpyplayer, and GStreamer provider modules/data are excluded from PyInstaller analysis and data collection.
- Post-build cleanup removes any Kivy video/GStreamer provider directories if a future hook reintroduces them.
- Current v0.0.12 packaging does not inject an external tools directory or PATH entry.
- Obsolete external-media packaging scripts and installer variant were removed or replaced by a native-only compatibility entry point.
- The portable scan rejects removed executable/library names and directories.

## Repository Search

Intentional production/build references outside docs/history/tests are limited to:

```text
config_migration.py: legacy names/keys needed to remove old configuration
scripts/Test-PortablePrivacy.ps1: forbidden artifact name pattern
AgoraLink.spec: excluded optional media-provider module/data names
scripts/package_release_v0_0_12.ps1: fail-closed cleanup paths
```

No production command builder, runtime branch, UI control, spec data entry, current README instruction, or process lookup remains.

## Portable Evidence

The v0.0.12 dry-run ZIP contained:

- FFmpeg executable count: 0.
- ffprobe executable count: 0.
- ffplay executable count: 0.
- `avcodec`/`avformat`/`avutil`/`swscale`/`swresample` named artifact count: 0.
- `ffpyplayer`/`gstplayer`/`gstreamer` named artifact count: 0.
- External media bundle directory count: 0.
- PDB count: 0.

Both staging and independently extracted trees passed the same scanner.

The final provider-hardened ZIP contains 1,447 entries, is 41,882,185 bytes, and has SHA-256 `A87CDDA5A127BD4B4F57994A2FC5534FD1D122A2C75C9142AC53116D3F364701`.

## Process Evidence

Residual media processes after deterministic validation: 0. The Python command-builder tests assert that only native `screen-send`/`screen-recv` commands are generated. No real WGC or dual-host session was run, so this report does not claim a hardware screen-sharing smoke result.

## Preserved Native Path

- Sender: WGC -> D3D11 conversion -> QSV/WMF H.264 -> AGM1 UDP.
- Receiver: AGM1 UDP -> reassembly/NACK -> WMF H.264 -> D3D11 render.
- Audio: WASAPI/native UDP when native audio capability is available.
