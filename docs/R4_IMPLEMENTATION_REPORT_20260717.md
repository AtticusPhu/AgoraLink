# AgoraLink R4 Implementation Report - 2026-07-17

## Executive Result

R4 source implementation is complete. The fixed Rust native sender default is now 1920x1080 at 60 FPS and 22 Mbps, with NACK repair and adaptive quality disabled. Smoothness adaptation now uses explicit Q0-Q4 and E1-E2 identities with adjacent resolution-first degradation and exact reverse recovery.

The deterministic implementation gate passed. Real WGC/QSV/D3D11, two-host LAN, 1800-second stability, and transition-close runtime tests were not executed and are not inferred from unit tests.

## Baseline and Branch

- R3 source checkpoint: `db44ee260fd84c04970409c0b1043d59c22b0583`
- R4 branch: `r4-default-adaptive-ladder`
- R3 executable SHA-256: `55AA6B837D1CA2DFCF6362D8BEE3CFA5A9998DC8F769FD76A415DBE02DB44B05`
- R3 baseline manifests:
  - `docs/R3_SOURCE_BASELINE_20260717.txt`
  - `docs/R3_SOURCE_BASELINE_20260717.json`

## Local Functional Commits

1. `dede316` - `test: define R4 default and adaptive ladder policy`
2. `acf778c` - `feat: set R4 native screen-send defaults`
3. `a286f92` - `refactor: model explicit R4 adaptive profiles`
4. `0d13bfd` - `feat: apply resolution-first adaptive ladder`
5. `c0fbdf2` - `feat: add R4 default screen preset`
6. `c8f6c8a` - `docs: document R4 screen defaults and adaptive policy`
7. `test: add R4 regression evidence` - containing commit for the final tests and this report

No push or remote pull request was performed.

## Modified Files

- `rust-native/agoralink_media/src/main.rs`
- `rust-native/agoralink_media/src/adaptive_quality.rs`
- `rust-native/agoralink_media/src/h264_send_probe.rs`
- `screen_runtime.py`
- `main_kivy.py`
- `tests/test_screen_runtime_r4.py`
- `README.md`
- `CHANGELOG.md`
- R4 evidence and report files under `docs/`

## Default Before and After

| Setting | R3 | R4 |
|---|---|---|
| width x height | 1280x720 | 1920x1080 |
| FPS | 30 | 60 |
| bitrate | 4 Mbps | 22 Mbps |
| repair | off | NACK |
| adaptive quality | off | off |
| encoder | auto | auto, QSV preferred with existing fallback |
| conversion | auto | auto, D3D11 preferred with existing CPU fallback |

The standalone `screen-recv` default playout delay remains 120 ms. The GUI R4 preset explicitly uses 250 ms.

## Explicit Bitrate Policy

Resolution order is unchanged from the requirement:

```text
explicit --bitrate-mbps
> explicit --quality-bpf
> default 22 Mbps
```

The 22 Mbps value is not a maximum. In smoothness mode, an initial bitrate `B` instantiates the entire ladder once:

| Profile | Resolution / FPS | Bitrate |
|---|---|---:|
| Q0 | 1920x1080 / 60 | B |
| Q1 | 1600x900 / 60 | B |
| Q2 | 1280x720 / 60 | B |
| Q3 | 1280x720 / 60 | min(B, 18) |
| Q4 | 1280x720 / 60 | min(B, 15) |
| E1 | 1280x720 / 45 | Q4 bitrate |
| E2 | 1280x720 / 30 | Q4 bitrate |

Profiles with identical physical values at low `B` advance their logical identity without issuing a redundant encoder update.

## State Transitions

```text
Normal degradation: Q0 -> Q1 -> Q2 -> Q3 -> Q4
Emergency only:     Q4 -> E1 -> E2
Recovery:           E2 -> E1 -> Q4 -> Q3 -> Q2 -> Q1 -> Q0
```

| Edge | Runtime classification |
|---|---|
| Q0 <-> Q1 | structural resolution change; existing session/profile transition path |
| Q1 <-> Q2 | structural resolution change; existing session/profile transition path |
| Q2 <-> Q3 | non-structural bitrate update; existing keyframe/update path and structural fallback retained |
| Q3 <-> Q4 | non-structural bitrate update; existing keyframe/update path and structural fallback retained |
| Q4 <-> E1 | structural FPS change; existing session/profile transition path |
| E1 <-> E2 | structural FPS change; existing session/profile transition path |

Pressure formulas, startup warmup, valid-window gates, mild/severe thresholds, stable-window counts, and cooldown durations were not changed. While `profile_transition_active=true`, feedback remains isolated, pressure windows reset, and no additional action or generation increment occurs.

## Telemetry

Existing fields remain. R4 additionally emits:

- `adaptive_profile_id`
- `adaptive_nominal_profile_id`
- `adaptive_profile_from`
- `adaptive_profile_to`
- `adaptive_profile_emergency`
- `adaptive_ladder_index`
- `adaptive_ladder_changes`

## GUI Preset and Migration

- New ID: `r4_default`
- New default ID: `r4_default`
- Tuple: 1920x1080, 60 FPS, 22 Mbps, 250 ms playout, NACK, adaptive off
- Missing `screen_native_preset`: use `r4_default`
- Existing `stable`, `recommended`, or `high_quality`: preserve the saved ID and values
- Unknown ID: warn once, persist fallback to `r4_default`

Legacy labels now explicitly identify their role. The sender command includes width, height, FPS, bitrate, repair, and adaptive mode. The receiver command includes the preset playout delay and repair mode.

## Verification

| Check | Result |
|---|---|
| R3 pre-change `cargo fmt --check` | PASS |
| R3 pre-change `cargo test --release` | PASS, 108 |
| R3 pre-change release build and self-test | PASS |
| R3 executable hash | MATCH |
| R4 `cargo fmt -- --check` | PASS |
| R4 `cargo test --release --locked --offline --jobs 1` | PASS, 121 passed, 0 failed, 0 ignored |
| R4 `cargo build --release --locked --offline --jobs 1` | PASS |
| R4 release executable self-test | PASS |
| Python `py_compile` | PASS |
| Python command-builder/migration tests | PASS, 6 |
| strict Clippy normal target | EXISTING_DEBT, 68 diagnostics |
| strict Clippy test target | EXISTING_DEBT, 70 diagnostics |
| new R4 Clippy diagnostics | 0 |

The project `.venv` launcher currently references a missing Python 3.12 base executable. Python checks used the Codex bundled Python runtime with `PYTHONPYCACHEPREFIX` outside the repository; no `__pycache__` content was added to the worktree.

## R4 Binary

- Path: `C:\Users\Attic\Desktop\U'W\6.0\UDP_Project\app_RUDP\AgoraLink\rust-native\agoralink_media\target\release\agoralink_media.exe`
- Size: 2,143,744 bytes
- SHA-256: `D0CEE722185FF1E294894557C008CA96B37FF8C75F909162DB7B658286C8AE9D`
- Different from R3: yes

## Runtime Validation

- Deterministic adaptive ladder, transition isolation, profile-transition, shutdown, WGC lifecycle, repair, AV, and packet tests: PASS as part of 121 tests.
- Local WGC/QSV/D3D11 loopback smoke: NOT RUN. Starting the native viewer required an elevated GUI execution approval that was rejected by the tool approval service.
- Two-host LAN and 1800-second fixed-profile stability: NOT RUN.
- Live Q0-Q4 degradation/recovery and transition-close test: NOT RUN.

No WGC, LAN, QSV, D3D11, or performance data is fabricated.

## Protected R3 Runtime Modules

No protocol, packet format, transition wire format, session rollover, STREAM_CLOSE, shutdown coordinator, worker join, NACK implementation/parameters, playout algorithm, WGC lifecycle, QSV/WMF implementation, D3D11 conversion implementation, decoder, renderer, audio, or AV-sync logic was changed.

`h264_send_probe.rs` changed only the final adaptive profile label source from inferred F0-F5 naming to the controller's explicit Q/E identity.

## Known Issues and Deferred Work

- Strict Clippy remains an audited repository-wide debt item; see `docs/R4_CLIPPY_BASELINE_COMPARISON_20260717.md`.
- Repair the local `.venv` base interpreter before relying on that environment for GUI runtime validation.
- Run the two-host LAN, 1800-second stability, live adaptive transition, and transition-close matrix before a release-quality runtime declaration.
- Main-file decomposition, NACK tuning, playout optimization, new pressure formulas, and media-runtime refactors remain deferred.
