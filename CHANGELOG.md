# Changelog

## v0.0.12 - 2026-07-20

- Rejected pre-session, foreign-peer, stale-session, malformed, zero-ID, and replayed native screen close control messages.
- Bounded H.264 packet counts and aggregate in-flight reassembly slots/payload before allocation.
- Fixed short UDP datagram classification to use the length returned by `recv_from`.
- Removed the external media runtime, capability, UI, configuration, packaging, and process-control paths; screen sharing now uses the bundled native Windows runtime only.
- Added one-time migration of legacy backend/path settings to the native backend.
- Added a local JSON stop channel and finite graceful, terminate, and force-kill deadlines for native screen subprocesses.
- Made Python test discovery deterministic and added a zero-test guard, locked Python/Rust environments, and Windows CI definitions.
- Removed PDB files from the public portable, added fail-closed portable privacy scanning, and added a separate privacy-gated symbols workflow.
- Preserved the R4 default profile, NACK repair, adaptive ladder semantics, WGC/QSV/WMF/D3D11 media behavior, chat, file transfer, discovery, and database contracts.
- Deferred the large Rust/Python module split to a later architecture-only change.

## R4 - 2026-07-17

- Changed the fixed Rust native `screen-send` default to 1920x1080, 60 FPS, 22 Mbps, NACK repair, and adaptive quality off.
- Added the explicit `r4_default` GUI preset without rewriting existing saved legacy preset IDs.
- Replaced inferred F0-F5 bitrate ranges with explicit Q0-Q4 and E1-E2 profile identities.
- Changed adaptive degradation to the adjacent resolution-first path: 1080p, 900p, 720p, 18 Mbps, then 15 Mbps.
- Changed recovery to the exact reverse adjacent path and limited emergency FPS reduction to sustained pressure after Q4.
- Preserved explicit bitrate priority, Intel QSV/software fallback, D3D11/CPU fallback, NACK internals, transition protocol, and media runtime behavior.

## v0.0.11

- Added About and Diagnostics information in Settings.
- Added current package flavor, Python runtime, Rust native availability, FFmpeg availability, app data directory, and log directory display.
- Added Rust native screen sharing presets for Stable, Recommended, and High Quality modes.
- Kept Native Lite video-only behavior explicit and disabled FFmpeg-only system audio expectations.
- Added safer diagnostic bundle metadata and config snapshot export.
- Added project README.

## v0.0.10

- Continued UI polish around compact tool layout and native screen backend capability display.
