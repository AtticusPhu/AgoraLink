# AgoraLink v0.0.12 Portable Report

Date: 2026-07-20

Branch: `audit-fixes-v0.0.12`

## Release Asset Status

`PASS`

The final public ZIP was built from clean source commit `6655d758281b0a7bed95845d83c58163b33350cd`. `BUILD_INFO.json` records that commit and CPython 3.12.10.

Final asset:

- Path: `_local_artifacts/V0_0_12_RELEASE/AgoraLink_v0.0.12_portable.zip`.
- Size: 43,626,481 bytes.
- SHA-256: `E16926EB865A2D9802AABE26B68718ACBFFA19271C4D8EFA838CCCAAA9A8AD6D`.
- Native executable SHA-256: `F0104BBC8946B6A03F8C7EEF7EB3CE03424C03B05F193989C320E3D509E1A5A6`.
- ZIP entries: 1,454.
- PDB, source, and removed external-media named files: 0.
- Staging/extraction privacy scans: PASS.
- Native self-test before/after extraction: PASS.

## Packaging Contract

Required public contents:

- `AgoraLink.exe`
- `_internal/tools/agoralink_media/agoralink_media.exe`
- `BUILD_INFO.json`
- `PORTABLE_CONTENTS.json`
- `SHA256SUMS.txt`
- `README.md`
- `CHANGELOG.md`
- required Python/Kivy/native runtime dependencies

Forbidden public contents:

- PDB/DMP/log/database/key/PIN/media-capture files
- user configuration, chat/transferred content, and test data
- Python/Rust/PowerShell/C/C++ source
- `_local_artifacts`, build, target, tests, and logs directories
- removed external-media executables/libraries/bundles
- current user-profile or source-checkout path prefixes

## Build Controls

- Output must be a child directory of `_local_artifacts`.
- Replacement requires both `-Force` and an exact ownership marker.
- Explicit `-Python` is fail closed; the script cannot silently select another interpreter.
- Source version must equal v0.0.12.
- Rust release build is locked/offline and uses path remapping for the public executable.
- PyInstaller must produce one native runtime and no PDB.
- Privacy scan runs on staging and independently extracted ZIP content.
- Native `self-test` runs from staging and extracted content.

## Dry-Run Evidence

- Python: official CPython 3.12.10 x64.
- Files scanned: 1,441.
- ZIP entries: 1,454.
- Size: 43,626,711 bytes.
- SHA-256: `60D4E040379F1DFA00894767A81BE924E3A7C2C33896CF75073DD71053DBF941`.
- PDB files: 0.
- Removed external-media names: 0.
- Source files: 0.
- Staging privacy scan: PASS.
- Extraction privacy scan: PASS.
- Native self-test: PASS before and after extraction.
- Local user/source paths: 0 findings.
- Third-party upstream build-path provenance: 8 files, recorded but not local.

## Symbols

`AgoraLink_v0.0.12_symbols.zip` was not created. The PDB privacy gate found local checkout/toolchain paths (`publishable=false`). The PDB remains a private local debugging artifact and is not part of the public portable.

## License

`USER_DECISION_REQUIRED`. No LICENSE file is required by the packaging gate until the owner selects a license, and automation did not create one.
