# AgoraLink R4 GUI Integration and Portable Report

Date: 2026-07-20
Release label: R4
Product version: v0.0.11 (unchanged)
Branch: `r4-default-adaptive-ladder`

## Result

The R4 Rust native media runtime is integrated into the GUI runtime lookup and the PyInstaller portable build. The final portable package was built from the fixed R4 release EXE/PDB, extracted to an independent verification directory, and launched successfully. No dual-host result is claimed in this report; G1-G5 and the append smoke remain user-executed tests.

## Local Commits

| Commit | Subject |
|---|---|
| `f662a8e408091b60613568f3072a8a522feeba3b` | `build: bundle R4 native media runtime` |
| `cbbb7aeec4780f56e167aa1698c843c322c43edb` | `test: verify r4_default GUI integration` |
| `0e1f65988694f163a267db1802464fc39435265a` | `test: add R4 GUI dual-host smoke harness` |

No push or remote PR was performed.

## Modified Files

- `AgoraLink.spec`
- `app_paths.py`
- `main_kivy.py`
- `screen_runtime.py`
- `tests/test_screen_runtime_r4.py`
- `scripts/package_r4_portable_20260720.ps1`
- `scripts/r4_gui_smoke/Prepare-R4GuiSmoke.ps1`
- `scripts/r4_gui_smoke/Collect-R4GuiSmoke.ps1`
- `docs/R4_GUI_PORTABLE_BASELINE_20260720.txt`
- `docs/R4_GUI_DUAL_HOST_SMOKE_20260720.txt`
- `docs/R4_GUI_PORTABLE_IMPLEMENTATION_REPORT_20260720.md`
- `docs/R4_GUI_PORTABLE_IMPLEMENTATION_REPORT_20260720.json`

No file under `rust-native/agoralink_media/src/` was changed.

## Native Runtime Integration

Authoritative release source:

`C:\Users\Attic\Desktop\U'W\6.0\UDP_Project\app_RUDP\AgoraLink\rust-native\agoralink_media\target\release\agoralink_media.exe`

Source EXE SHA-256:

`D0CEE722185FF1E294894557C008CA96B37FF8C75F909162DB7B658286C8AE9D`

Source PDB SHA-256:

`0761475AD6D852B4A55A02D05C51706FCC838F9F0BBDE2FB5D40135EA32ECC2E`

The packaging script verifies the source hash and PDB, removes only its marker-owned staging directory, copies the two files to controlled staging, and verifies the EXE hash again. `AgoraLink.spec` consumes that staging directory through `AGORALINK_NATIVE_RUNTIME_DIR`.

Bundled paths:

- `_internal/tools/agoralink_media/agoralink_media.exe`
- `_internal/tools/agoralink_media/agoralink_media.pdb`

Runtime lookup order is:

1. Frozen `_internal/tools/agoralink_media`.
2. Frozen `_MEIPASS/tools/agoralink_media` or executable-adjacent tools path.
3. Source `rust-native/agoralink_media/target/release`.
4. Supported project tools fallback.
5. `PATH`.

Every selected native executable is checked against the fixed R4 SHA-256 before capability probing or process launch. The path and SHA-256 are written to the native screen debug log when a native process starts.

## Stale Binary Control

Historical binaries in existing `dist/`, `tools/`, or `_local_artifacts/` locations were not deleted because they are outside this release's marker-owned output and may be user evidence. They were excluded by controlled staging and by the portable policy scan. The final portable contains zero files with the forbidden R3 SHA-256:

`55AA6B837D1CA2DFCF6362D8BEE3CFA5A9998DC8F769FD76A415DBE02DB44B05`

## GUI Configuration Verification

| Scenario | Result | Evidence |
|---|---|---|
| Fresh config defaults to `r4_default` | PASS | `test_default_preset_is_complete_r4_tuple` |
| Existing `stable` remains selected | PASS | `test_existing_valid_preset_ids_remain_selected` |
| Existing `recommended` remains selected | PASS | same test |
| Existing `high_quality` remains selected | PASS | same test |
| Invalid preset falls back to `r4_default` | PASS | `test_invalid_preset_falls_back_and_warns_once` |
| Invalid preset warning is emitted once | PASS | same test |
| GUI persists the corrected preset | PASS (code path verified) | `RUDPTransferRoot.__init__` updates `gui_config` and calls `save_gui_config` |

`r4_default` resolves to 1920x1080, 60 FPS, 22 Mbps, NACK repair, adaptive quality off, encoder auto, conversion backend auto, D3D11 render backend, and 250 ms receiver playout delay. Legacy preset bitrate values remain unchanged.

## Command Construction Verification

Sender command tests confirm:

`--width 1920 --height 1080 --fps 60 --bitrate-mbps 22 --repair nack --adaptive-quality off --encoder auto --convert-backend auto`

Receiver command tests confirm:

`--playout-delay-ms 250 --repair nack --render-backend d3d11`

Result: PASS.

## Automated Validation

- Python `py_compile`: PASS.
- `tests.test_screen_runtime_r4`: 10 tests, PASS.
- `screen_runtime.py --self-test`: 44 checks, PASS.
- Package-time native self-test from the portable directory: PASS.
- Native self-test after ZIP extraction: PASS.
- PowerShell parser checks for package and dual-host harness scripts: PASS.

The final build used the existing project venv: Python 3.12.10, Kivy 2.3.1, PyInstaller 6.20.0, and its installed pywin32 runtime. No dependency was installed or upgraded. An initial sandbox-only attempt with a fallback Python failed because `.pth` processing did not expose pywin32 correctly; it produced no release and was replaced by the successful project-venv build.

## Dual-Host GUI Smoke

| Test | Status |
|---|---|
| G1 GUI launch and default values | USER_EXECUTION_REQUIRED |
| G2 dual-host screen sharing start | USER_EXECUTION_REQUIRED |
| G3 sender GUI stop | USER_EXECUTION_REQUIRED |
| G4 restart without closing GUI | USER_EXECUTION_REQUIRED |
| G5 receiver GUI/window stop | USER_EXECUTION_REQUIRED |

Preparation and collection scripts are provided under `scripts/r4_gui_smoke/`. They verify the portable/native hash, capture process/UDP/DMP snapshots, redact sensitive configuration fields, collect logs, and create per-host evidence ZIPs. They do not change firewall rules or terminate processes.

## Append Feature Definition

The current GUI has no literal `Append` button. The existing append behavior is starting another independent chat file transfer by selecting `Send file` again while a prior chat file transfer is active. The path is:

`send_file_to_current_chat -> _send_file_path_to_current_chat -> _start_file_transfer_for_message`

Each selection creates a distinct message, transfer task, card, and worker path. Protocol semantics were not changed. The dual-host append smoke and final SHA-256 comparison are USER_EXECUTION_REQUIRED.

## Portable Artifact

Portable directory:

`C:\Users\Attic\Desktop\U'W\6.0\UDP_Project\app_RUDP\AgoraLink\_local_artifacts\R4_GUI_PORTABLE_20260720\AgoraLink_R4_portable_20260720`

Portable ZIP:

`C:\Users\Attic\Desktop\U'W\6.0\UDP_Project\app_RUDP\AgoraLink\_local_artifacts\R4_GUI_PORTABLE_20260720\AgoraLink_R4_portable_20260720.zip`

ZIP SHA-256:

`41CBAEE0ADC3F0F542811D783A7B84B66F1A011C56FD4F82D39FAEDD8B18E942`

ZIP size: 46,120,474 bytes.
Extracted portable size: 102,738,988 bytes across 1,486 files.
`PORTABLE_CONTENTS.json` records 1,484 payload entries; the manifest and checksum file themselves are generated afterward.

Key hashes:

| File | SHA-256 |
|---|---|
| `AgoraLink.exe` | `2FA0CC2C38AA28891D71096342422C4B82D315CEA381054381CA9EC8D54081FB` |
| `_internal/tools/agoralink_media/agoralink_media.exe` | `D0CEE722185FF1E294894557C008CA96B37FF8C75F909162DB7B658286C8AE9D` |
| `_internal/tools/agoralink_media/agoralink_media.pdb` | `0761475AD6D852B4A55A02D05C51706FCC838F9F0BBDE2FB5D40135EA32ECC2E` |
| `BUILD_INFO.json` | `23F01450702622010E4C54B34CBBDE54B5B56CAF65601FF9A46FB3D8AA48C36C` |
| `PORTABLE_CONTENTS.json` | `81901F94B88B7D2BF0AA564149C0A0637E12130F14745DC417C750C8867A3D94` |

The package contains no FFmpeg executable, `.jsonl`, `.dmp`, database, key, PIN, audio probe, source `target`, or `_local_artifacts` payload. It includes the R4 native EXE and PDB exactly once each.

## Independent Portable Launch

The ZIP was extracted to a fresh `portable_verify` directory. The extracted `AgoraLink.exe` was launched with isolated `LOCALAPPDATA` and Kivy state. A real window became ready, `CloseMainWindow()` was accepted, and the process exited normally with code 0.

- Window ready: true.
- Graceful close: true.
- Exit code: 0.
- New crash dumps: 0.
- Residual `AgoraLink.exe`/`agoralink_media.exe` processes: 0.
- Kivy runtime log confirms the modules were loaded from the extracted portable `_internal` directory.

Evidence: `_local_artifacts/R4_GUI_PORTABLE_20260720/independent_gui_launch/result.json`.

## Known Non-Blocking Items

1. Dual-host G1-G5 and append transfer checks require two real Windows hosts and remain USER_EXECUTION_REQUIRED.
2. A native process path/hash log entry is created when screen sharing starts; live validation of that line belongs to G2. The frozen path resolution and hash enforcement are covered by automated tests and package self-tests.
3. The release intentionally pins one R4 native EXE hash. Replacing the native runtime requires an explicit release metadata update; an arbitrary binary is rejected.
4. Pre-existing unrelated worktree changes and historical artifacts were preserved and excluded from all R4 commits.
