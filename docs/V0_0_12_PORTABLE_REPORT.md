# AgoraLink v0.0.12 Portable Report

Date: 2026-07-21

Branch: `audit-fixes-v0.0.12`

## Candidate Status

`PORTABLE_VERIFICATION_PASS`

The release-candidate portable was rebuilt from pushed source commit `ba4369dfffc0d3efdba622050586f39c0c16404d`. That commit is resolvable on GitHub, and `BUILD_INFO.json` records it. Overall release readiness remains `BLOCKED` by manual GUI/dual-host/file-append gates; this report only certifies the package artifact.

## Final Candidate

- Path: `_local_artifacts/V0_0_12_RELEASE_FINAL_ba4369d/AgoraLink_v0.0.12_portable.zip`.
- Size: 41,881,098 bytes.
- SHA-256: `9927AE652064670E9CE771B4BA93D0F13B38BA33DDCE697E60F7ECFAB4CCF479`.
- Hash file: `_local_artifacts/V0_0_12_RELEASE_FINAL_ba4369d/AgoraLink_v0.0.12_portable.sha256.txt` (matches).
- Native executable SHA-256: `DFA7A6F4B4F3D4C71CD9F7B76E754F4D0C7EAB8D0F24963DCACCA8BE15A70A17`.
- BUILD_INFO source commit: `ba4369dfffc0d3efdba622050586f39c0c16404d`.
- Python: official CPython 3.12.10 x64.
- Rust: 1.96.0 stable MSVC x64.
- ZIP entries: 1,447.
- Files scanned in staging and extracted trees: 1,434 each.
- Third-party upstream build-path provenance: 2 files; current user/source paths: 0 findings.

## Required Contents

- `AgoraLink.exe`.
- `_internal/tools/agoralink_media/agoralink_media.exe`.
- `BUILD_INFO.json`.
- `PORTABLE_CONTENTS.json`.
- `SHA256SUMS.txt`.
- README and changelog.
- Required Python/Kivy/native dependencies.

## Forbidden Content Results

- FFmpeg/ffprobe/ffplay and related external-media files: 0.
- Optional ffpyplayer/GStreamer/gstplayer providers: 0.
- PDB: 0.
- DMP: 0.
- Python/Rust/PowerShell/C/C++ source: 0.
- User configuration, databases, keys, PINs, logs, and captured/transferred media: 0 findings.
- Current user-profile/source checkout prefixes: 0 findings.

Both staging and independent extraction passed the privacy scanner. Native `self-test` passed before compression and from the extracted archive.

## Independent Launch

Evidence:

`_local_artifacts/V0_0_12_FINAL_VALIDATION/portable_gui_launch_ba4369d`

The extracted `AgoraLink.exe` created a real top-level window. The harness posted `WM_CLOSE`; it exited with code 0 in 3,430ms. Forced cleanup was not used and no residual application/native process remained.

This proves independent startup/close of the packaged GUI. It does not replace the manual authenticated active-screen-share stop/restart test.

## Build Controls

- Output is restricted to a marker-owned child of `_local_artifacts`.
- Explicit `-Python` selection fails closed.
- Source version must equal v0.0.12.
- Rust release build is locked/offline and applies path remapping.
- PyInstaller must produce one native runtime and no PDB.
- The same forbidden-content/privacy gate runs before compression and after extraction.
- The ZIP hash file was independently re-read and matched the computed hash.

## Symbols and License

No symbols archive was created because the PDB contains local checkout/toolchain paths and is not publishable. License status remains `USER_DECISION_REQUIRED`; automation did not select a repository license.
