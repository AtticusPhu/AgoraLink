# AgoraLink R4 Codebase Baseline Audit

Date: 2026-07-17
Repository: `C:\Users\Attic\Desktop\U'W\6.0\UDP_Project\app_RUDP\AgoraLink`
Audit mode: read-only; only this Markdown report and the companion JSON report are added.
Branch / HEAD: `main` / `5315a094e7dd14404eb3fa29a13ba9625f8726ba`

## 1. Executive conclusion

### Facts

1. The current Rust `screen-send` CLI default is **1280x720, 30 FPS, 4 Mbps, encoder=auto, conversion=auto, repair=off, adaptive=off**, not the fixed R4 product default. Evidence: `rust-native/agoralink_media/src/main.rs`, `parse_screen_send_args`, lines 1550-1569 and 1682-1712.
2. The Python GUI default native preset is `stable`, currently **1280x720, 30 FPS, 20 Mbps, playout 120 ms, repair=off**. Evidence: `screen_runtime.py`, `DEFAULT_NATIVE_SCREEN_PRESET` and `NATIVE_SCREEN_PRESETS`, lines 38-69; `main_kivy.py`, GUI config loading, lines 2139-2154.
3. User-supplied `--bitrate-mbps` already wins over `--quality-bpf` and the fallback default. Evidence: `rust-native/agoralink_media/src/bitrate.rs`, `BitrateSelection::resolve`, lines 27-58; self-test lines 138-164.
4. Adaptive quality defaults to `off`. When enabled, it is a **mixed dynamic model**, not an explicit Q0-Q4 ladder: profile names are inferred from resolution/FPS/bitrate, bitrate bounds are dynamic, and bitrate reductions use 0.88/0.92 factors. Evidence: `adaptive_quality.rs`, lines 4-31, 129-174, 949-1077.
5. The current general downgrade path is bitrate-first, then resolution, then emergency FPS; render-only pressure can reduce FPS first and decoder-only pressure can reduce resolution first. The current recovery order is FPS, resolution, bitrate. Evidence: `adaptive_quality.rs`, `degrade`, lines 949-1077; `recover`, lines 1080-1148.
6. Bitrate-only changes normally update the running encoder in place and request a keyframe. Resolution/FPS changes rebuild the pipeline, create a new session, and use the MPRF/MPAK transition path. A failed runtime bitrate update falls back to a structural rebuild. Evidence: `capture_encode_probe.rs`, lines 883-923; `h264_send_probe.rs`, lines 140-148, 1242-1542, 2841-3087.
7. The transition, session rollover, WGC RAII, STREAM_CLOSE, NACK, playout, shutdown, worker join, QSV, D3D11, and WMF paths are modular and do not need behavior changes for R4.
8. `main.rs` is large (2918 lines), but most media runtime responsibilities are already delegated. R4 does not require a large split. The recommended approach is **C: implement R4 without splitting `main.rs`, then perform a separate structural refactor**.
9. Verification results for the current worktree are: `fmt --check` PASS; `cargo test --release` PASS (108/108); `cargo build --release` PASS; executable self-test PASS; strict Clippy FAIL because the existing tree emits approximately 70 lint diagnostics under `-D warnings`.
10. The current release executable is 2,143,232 bytes and has SHA-256 `55AA6B837D1CA2DFCF6362D8BEE3CFA5A9998DC8F769FD76A415DBE02DB44B05`, matching the supplied frozen R3 hash.

### Recommendation

Implement R4 as a small, test-first change centered on `adaptive_quality.rs`, `main.rs`, and `screen_runtime.py`. Preserve the established transition and media runtime modules. Introduce an explicit adjacent Q0-Q4 ladder, keep emergency FPS states separate, and make GUI/CLI defaults converge on the fixed product profile.

### Validation gate issue

The required strict Clippy command does not currently pass. This is pre-existing lint debt rather than an R4 functional failure, but it must either be fixed in a separate baseline cleanup or explicitly baselined before R4 can claim a fully green validation gate. It must not be mixed into the adaptive behavior commits.

## 2. Audit method and evidence labels

- **Fact**: directly observed in the current worktree, command output, or retained evidence.
- **Inference**: conclusion derived from multiple facts; the evidence is listed.
- **Recommendation**: proposed R4 implementation or sequencing; no code was changed by this audit.

The repository was already dirty before the audit. No pre-existing changes were reverted or rewritten. Cargo validation only touched ignored build outputs under `target/`.

## 3. Repository and build baseline

### 3.1 Git baseline before audit

| Item | Value |
|---|---|
| Repository detected | Yes |
| Branch | `main` |
| HEAD | `5315a094e7dd14404eb3fa29a13ba9625f8726ba` |
| Dirty before audit | Yes |
| Tracked changes before audit | 23 paths; `+8834/-988` in the recorded diff stat |
| Untracked content before audit | Present, including Rust modules, reports, artifacts, and validation scripts |
| Audit-created product changes | None |

Pre-existing tracked changes recorded before the audit:

```text
.gitignore
app_paths.py
diagnostic_export.py
docs/RUST_NATIVE_AV_CODE_AUDIT.md (deleted)
main_kivy.py
rust-native/agoralink_media/Cargo.toml
rust-native/agoralink_media/src/capture_encode_probe.rs
rust-native/agoralink_media/src/capture_probe.rs
rust-native/agoralink_media/src/d3d11_nv12_renderer.rs
rust-native/agoralink_media/src/gpu_nv12_capture.rs
rust-native/agoralink_media/src/h264_reassembly.rs
rust-native/agoralink_media/src/h264_recv_dump.rs
rust-native/agoralink_media/src/h264_recv_view.rs
rust-native/agoralink_media/src/h264_send_probe.rs
rust-native/agoralink_media/src/main.rs
rust-native/agoralink_media/src/repair.rs
rust-native/agoralink_media/src/video_renderer.rs
rust-native/agoralink_media/src/wgc_latest_capture.rs
rust-native/agoralink_media/src/win32_gdi_viewer.rs
rust-native/agoralink_media/src/wmf_h264_decoder.rs
rust-native/agoralink_media/src/wmf_h264_encoder.rs
screen_runtime.py
screen_share_presenter.py
```

The full untracked baseline is preserved by Git and is not reproduced as an implementation target. Notable untracked Rust modules include `adaptive_quality.rs`, `profile_transition.rs`, `shutdown.rs`, `media_control.rs`, `audio_udp.rs`, and supporting R3 hardening modules.

### 3.2 Source, worktree build, and frozen R3 distinction

| Baseline | Meaning | Evidence | Result |
|---|---|---|---|
| Current source | Files currently present in the dirty worktree | Git status plus source inspection | Audited as the implementation source of truth |
| Current worktree build | `cargo build --release` against the current source/fingerprint graph | `target/release/agoralink_media.exe` | PASS; 2,143,232 bytes; hash matches R3 |
| Frozen R3 delivery | Historical fixed executable identified by supplied hash and retained validation evidence | `_local_artifacts/phase6_r3_20260717_123415/phase6_final_assessment.json` | Hash `55AA...B05`; Phase 6 accepted partial pass |

There is no separately named frozen executable under `_local_artifacts` to compare byte-for-byte. The current build output itself matches the frozen hash exactly. Cargo reported the release target as fresh, so this establishes artifact identity for the currently resolved source fingerprint, but the historical delivery copy is not independently present.

### 3.3 Frozen R3 evidence retained in the repository

- Phase 6 assessment: `phase6_final_assessment.json` records 20/20 capture, 10/10 CPU loopback, 20/20 D3D11 loopback, exact-size render tests, 10/10 Ctrl+Break, no new dumps, no residual processes, no verifier stops, no native screen errors, and no shutdown failures.
- The WM_CLOSE case was explicitly classified `INVALID_TEST_TIMING`, not a product failure; the receiver still stopped with `window_closed` and clean worker joins.
- The 600-second sender evidence at `_local_artifacts/phase8_22m/R3_PHASE8_22M_20260717_R1/sender/P8_01_1080P60_22M_600S.jsonl` ends with 35,986 frames, 59.90 FPS, 22.525 Mbps, 30,311 repair packets, 2.742 ms average encode time, 0.567 ms average GPU conversion time, Intel Quick Sync, D3D11 conversion, adaptive off, and duration stop.
- Receiver-side aggregate values in the task baseline (58.95 rendered FPS, approximately 0.0064% estimated loss, approximately 99.961% complete after repair) are historical acceptance evidence; the receiver raw file was not present in the inspected Phase 8 sender directory.

## 4. Rust source inventory

There is no `rust-native/agoralink_media/tests/` directory. All 108 Cargo tests are inline `#[cfg(test)]` modules under `src/`.

| Rust source file | Lines |
|---|---:|
| `adaptive_quality.rs` | 2139 |
| `async_mft_wait.rs` | 260 |
| `audio_capture_probe.rs` | 466 |
| `audio_timeline.rs` | 134 |
| `audio_udp.rs` | 2601 |
| `av_sync.rs` | 501 |
| `bench_reassembly.rs` | 133 |
| `bgra_to_nv12.rs` | 379 |
| `bitrate.rs` | 171 |
| `callback_lifecycle.rs` | 378 |
| `capture_encode_probe.rs` | 1233 |
| `capture_probe.rs` | 451 |
| `color_spec.rs` | 273 |
| `color_test_pattern.rs` | 367 |
| `d3d11_nv12_renderer.rs` | 600 |
| `decoded_frame_renderer.rs` | 117 |
| `display_capability.rs` | 465 |
| `encode_probe.rs` | 240 |
| `fec.rs` | 215 |
| `frame_rate_policy.rs` | 373 |
| `gpu_convert_probe.rs` | 355 |
| `gpu_nv12_capture.rs` | 1061 |
| `h264_annex_b.rs` | 445 |
| `h264_file_viewer.rs` | 299 |
| `h264_reassembly.rs` | 1708 |
| `h264_recv_dump.rs` | 413 |
| `h264_recv_view.rs` | 4141 |
| `h264_send_probe.rs` | 4114 |
| `main.rs` | 2918 |
| `media_clock.rs` | 193 |
| `media_control.rs` | 1073 |
| `nv12_synthetic.rs` | 76 |
| `nv12_to_bgra.rs` | 160 |
| `playout_buffer.rs` | 174 |
| `profile_transition.rs` | 1246 |
| `repair.rs` | 364 |
| `sender_scheduling.rs` | 281 |
| `shutdown.rs` | 793 |
| `udp_socket.rs` | 96 |
| `video_renderer.rs` | 195 |
| `wgc_latest_capture.rs` | 710 |
| `win32_gdi_viewer.rs` | 954 |
| `wmf_h264_decoder.rs` | 736 |
| `wmf_h264_encoder.rs` | 1661 |
| `wmf_probe.rs` | 308 |

### 4.1 `main.rs` structural counts

| Metric | Count |
|---|---:|
| Lines | 2918 |
| Module declarations | 44 |
| Top-level functions | 52 |
| Top-level structs | 4 |
| Top-level enums | 1 |

Top-level functions whose span to the next top-level function exceeds 100 lines:

| Function | Approximate lines | Span |
|---|---:|---|
| `main` | 141 | 311-451 |
| `parse_capture_encode_probe_args` | 101 | 725-825 |
| `parse_h264_send_probe_args` | 152 | 826-977 |
| `parse_h264_recv_view_args` | 226 | 1324-1549 |
| `parse_screen_send_args` | 167 | 1550-1716 |
| `parse_screen_recv_args` | 273 | 1717-1989 |
| `run_self_test` | 342 | 2574-2915 |

These are approximate lexical spans; they are suitable for responsibility sizing but are not complexity metrics.

## 5. `main.rs` responsibility map

The task list names several responsibilities that are no longer directly implemented in `main.rs`. The table distinguishes dispatch/ownership from the actual implementation module.

| Responsibility | Main entry / lines | Actual implementation dependencies | R4-related | Move during R4 |
|---|---|---|---|---|
| Module wiring | module declarations, 10-53 | 44 `src` modules | Indirect | No |
| CLI command model | `Command`, 174-207 | all subcommand config types | Yes | No |
| Process entry and JSON/error dispatch | `main`, 311-450 | sender/receiver/probe modules | Indirect | No |
| Global CLI dispatch | `parse_args`, 452-492 | all parser functions | Yes | No |
| `screen-send` parsing/defaults | `parse_screen_send_args`, 1550-1714 | `bitrate`, `adaptive_quality`, `h264_send_probe`, encoder/converter/repair enums | Direct | No; edit locally |
| `screen-recv` parsing/defaults | `parse_screen_recv_args`, 1717-1988 | `h264_recv_view`, renderer, repair, display capability | Only default matrix | No |
| Adaptive option parsing | `parse_adaptive_sender_option`, 1990-2079 | `AdaptiveRuntimeConfig` | Direct | No |
| Screen sender runtime scheduling | dispatch at 392-396 | `h264_send_probe::run` | Direct through actions | Already outside `main.rs` |
| Screen receiver runtime scheduling | dispatch at 398-402 | `h264_recv_view::run` | Protected | Already outside `main.rs` |
| WGC initialization/capture | no direct implementation | `wgc_latest_capture`, `gpu_nv12_capture`, `capture_encode_probe` | No | No |
| QSV/WMF encoder initialization | no direct implementation | `wmf_h264_encoder`, capture pipeline | Default selection only | No |
| D3D11 conversion | no direct implementation | `gpu_nv12_capture`, `capture_encode_probe` | Default selection only | No |
| WMF decoding | no direct implementation | `wmf_h264_decoder`, `h264_recv_view` | No | No |
| UDP video send / receive | legacy prototype at 2345-2556; production delegated | `h264_send_probe`, `h264_recv_view`, `h264_reassembly`, `udp_socket` | No | Defer legacy extraction |
| NACK / repair | enum/default parsing at 1566, 1737; runtime delegated | `repair`, sender/receiver runtime | Product mode default only | No runtime change |
| Profile transition | no protocol implementation in `main.rs` | `profile_transition`, `media_control`, `h264_send_probe` | Must remain stable | No |
| Session rollover | no direct implementation | `profile_transition`, `h264_send_probe`, `h264_recv_view` | Triggered by resolution/FPS | No |
| Shutdown / worker join | no direct implementation | `shutdown`, sender/receiver modules | Protected | No |
| JSONL telemetry | entry errors in `main`; runtime stats in modules | sender/receiver/probe modules | Add profile labels only if needed | No structural move |
| Probe/test subcommands | parsers 494-1549; dispatch 323-391 | capture, encode, viewer, audio, benchmark modules | Test fixtures, not defaults | No |
| Legacy AGM1 synthetic prototype | `MediaPacket`, `Reassembler`, `run_sender`, `run_receiver`, 74-308 and 2345-2556 | std UDP only | No | Defer |
| Aggregate self-test | `run_self_test`, 2574-2914 | module `run_self_test` functions | Direct test additions | Keep for R4 |

### High-coupling functions

- `main` couples CLI dispatch, error policy, stdout/stderr behavior, and all subcommands. It is stable dispatch code, not media runtime code.
- `parse_screen_send_args` couples product defaults, validation, encoder/converter/repair selection, bitrate resolution, audio, and adaptive controls. This is the primary R4 default edit point.
- `parse_screen_recv_args` couples receiver defaults across repair/playout/render/audio; R4 should avoid touching it except if a product default matrix explicitly requires it.
- `run_self_test` couples all module self-tests. R4 tests should be added without reorganizing this function.

## 6. Default-value source matrix

Each cell states current value, source, symbol/line, and whether it is a product default.

| Setting | Rust CLI | Python/GUI | Config file | Test scripts | Help text |
|---|---|---|---|---|---|
| width | `1280`, `main.rs:1557`, `parse_screen_send_args`; **standalone product default** | `stable=1280`, `recommended/high=1920`, `screen_runtime.py:40-69`; **GUI product presets** | stores preset id only, `main_kivy.py:2152-2154,5825` | Phase fixtures use 1280/1920; not defaults | probe examples show 1280; screen help omits explicit default, `main.rs:2320,2335` |
| height | `720`, `main.rs:1558`; **standalone product default** | `stable=720`, recommended/high=1080, same preset table | preset id only | fixture-specific | same as width |
| fps | `30`, `main.rs:1554`; **standalone product default** | `stable=30`, recommended/high=60, `screen_runtime.py:45,55,65` | preset id only | validation cases use 30/60 | help says adaptive max FPS 60 but does not state screen-send core default, `main.rs:2338` |
| bitrate | fallback `4.0`, `main.rs:1682-1689`; explicit overrides BPF, `bitrate.rs:27-58`; **standalone product default** | 20/50/80 Mbps, `screen_runtime.py:46,56,66`; **GUI preset values** | preset id only | 8/35/50/etc. are fixtures | probe examples 4/8; README repeats 20/50/80; no R4 22 default |
| adaptive-quality | `off`, `AdaptiveRuntimeConfig::default` and `AdaptiveConfig::default`, `h264_send_probe.rs:27-44`, `adaptive_quality.rs:230-247`; **product default** | sender command does not pass the option, so Rust default applies | no GUI key found | scripts explicitly pass `off`; fixture | help lists `off` default, `main.rs:2338` |
| encoder | `Auto`, `main.rs:1560`; prefers Intel QSV but can fall back, `wmf_h264_encoder.rs:60-82,401-430,938-1008`; **product default** | command omits encoder, `screen_runtime.py:849-867`, so Rust `Auto` applies | no native encoder key found | tests often pass `auto` | examples say `auto` |
| convert-backend | `Auto`, `main.rs:1561`; tries D3D11 then CPU, `capture_encode_probe.rs:2-15,366-410`; **product default** | command omits converter, so Rust `Auto` applies | no native converter key found | fixtures commonly force CPU/D3D11 | examples say `auto` |
| repair | `off`, `main.rs:1566`; **standalone product default** | `stable=off`, recommended/high=`nack`, `screen_runtime.py:47-68`; GUI preset drives sender and receiver | preset id only | validation fixtures use `nack` | screen help lists choices but not core default |
| playout-delay | receiver `120 ms`; audio-on implicit `250 ms`, `main.rs:1728,1948-1955`; **receiver default** | stable 120, recommended 250, high 300, passed explicitly in `screen_runtime.py:807-834` | preset id only | fixture-specific | receiver help default 120; task says R4 must not change this behavior |

### 6.1 Current vs fixed R4 product default

| Item | Current standalone Rust | Current GUI default | Fixed R4 target | Required action |
|---|---|---|---|---|
| Resolution | 1280x720 | 1280x720 (`stable`) | 1920x1080 | Change Rust and GUI default source |
| FPS | 30 | 30 | 60 | Change Rust and GUI default source |
| Bitrate | 4 Mbps | 20 Mbps | 22 Mbps | Change both; retain explicit override |
| Encoder | `auto` | omitted -> `auto` | Intel QSV | Set explicit default semantics or pass explicit GUI argument |
| Conversion | `auto` | omitted -> `auto` | D3D11 | Set explicit default semantics or pass explicit GUI argument |
| Repair | off | off | NACK | Change sender/receiver GUI preset and standalone sender default as required |
| Adaptive | off | omitted -> off | off | Already correct |

`auto` is not semantically identical to a hard Intel-QSV or D3D11 default because it permits fallback. The R4 implementation task should state whether product startup must fail when those capabilities are absent or whether “default” means preferred-with-fallback. The fixed profile values themselves are not under debate.

### 6.2 Literal classification

- Product runtime defaults: `main.rs:1550-1714`, `main.rs:1717-1988`, `screen_runtime.py:38-69`.
- GUI recommendation values: 20/50/80 Mbps in `screen_runtime.py:40-69` and `README.md:10-12`.
- Validation fixtures: `scripts/package_native_media_test.ps1` defaults to 60 FPS/50 Mbps/250 ms and explicitly builds 1080p/QSV-auto/D3D11/NACK command lines; this is not a product default.
- WGC validation fixtures: `scripts/wgc_raii_validation/Run-Phase3.ps1` and `Run-Phase4.ps1` use fixed 720p/1080p, 30 FPS, 8 Mbps cases to validate resource and shutdown behavior; they are not product defaults.
- Adaptive boundary/legacy values: 42/50, 34/42, 28/36, 20/28, 14/22, and 8/14 in `adaptive_quality.rs:165-173` are dynamic profile bounds, not UI recommendations.
- Test-only values: 50, 40, 34, 20, and 10 Mbps in `adaptive_quality.rs:1509-1615` are fixtures asserting the current strategy.
- Encoder internal defaults: `EncoderChoice::Auto` and Microsoft fallback behavior belong to capability selection, not bitrate policy.

## 7. Current adaptive-quality state machine

### 7.1 Inputs and eligibility

`AdaptiveSnapshot` and `classify_pressure` consume sender/receiver feedback including actual throughput, packet loss, repairs, repair deadline misses, damaged GOPs, pacing/send latency, decoder input FPS, decode queue drops, render throughput, present cadence, and transition status. Evidence: `adaptive_quality.rs:406-436,1273-1343`.

Eligibility gates in `observe_windowed`:

1. `mode=off` -> Disabled, no action.
2. Active profile transition -> feedback classified but ignored; no action.
3. Stale or ineligible feedback -> no action.
4. Startup warmup default 5 seconds and at least 5 valid transport windows are required.
5. Severe pressure requires 3 windows, mild pressure 5 windows, and recovery requires 15 stable windows by default.

Evidence: `adaptive_quality.rs:213-247,812-898`.

### 7.2 Current state-transition table

| Current state | Trigger / guard | Next state | Action |
|---|---|---|---|
| Disabled | mode remains off | Disabled | none |
| Disabled/Startup | smoothness enabled but startup <5 s or valid-window gate incomplete | Startup | collect baseline only |
| Any eligible | `profile_transition_active=true` | ProfileTransition | suppress adaptation; reset pressure windows |
| Startup/Stable | one mild eligible window | MildPressure | increment mild streak; no action until 5 |
| Startup/Stable | one severe eligible window | SeverePressure | increment severe streak; no action until 3 |
| MildPressure | mild streak >=5 | Cooldown or change state | call current `degrade(false)` |
| SeverePressure | severe streak >=3 | Cooldown/change; possible EmergencyFpsReduction | call current `degrade(true)` |
| Stable | stable streak >=15 | Recovering/Cooldown | call current `recover()` |
| Any action | action applied | cooldown represented through timestamps/reason | reset valid window baseline and pressure streaks |
| Cooldown | dimension cooldown not elapsed | Cooldown | no action; increment blocked counter |
| EmergencyFpsReduction | sustained severe >=5 at min resolution/floor and FPS > min | post-change cooldown | 60->45->30 |

### 7.3 Pressure classification (unchanged for R4)

- Network severe: repair deadline miss, damaged GOP, or send error.
- Network mild: packet loss, at least four repair resends, pacing P95 > 0.5 frame budget, or send syscall P95 > 0.25 frame budget.
- Decoder severe/mild: decoder input ratio below 0.80/0.90 or queue drops.
- Render severe/mild: render ratio below 0.75/0.90 or sustained present pressure.
- Transition is severe/ineligible for action when active.

Evidence: `adaptive_quality.rs:1273-1343`. R4 must not alter these formulas.

### 7.4 Current downgrade and recovery behavior

Current downgrade decision order is conditional:

1. Render-only pressure: FPS first (`render-pressure-fps-first`).
2. Decoder-only pressure: resolution first (`decoder-pressure-resolution-first`).
3. General/network/mixed pressure: bitrate first while above dynamic floor, using factor 0.88 (severe) or 0.92 (mild).
4. Once the dynamic bitrate floor is reached: resolution 1080p -> 900p -> 720p.
5. At minimum resolution after sustained severe pressure: emergency FPS 60 -> 45 -> 30.

Current recovery is FPS -> resolution -> bitrate (multiplying bitrate by 1.06 up to nominal). Evidence: `adaptive_quality.rs:949-1148`.

This is incompatible with the fixed R4 order. It also creates a concrete 22 Mbps inconsistency: 1920x1080/22 is named `F1`, whose current floor is 34 Mbps; 720p60/22 is `F3`, whose floor is 20 Mbps, so the current model cannot express exact Q3=18 or Q4=15 without replacing the bounds-based strategy.

### 7.5 Current profile generation model

Classification: **mixed resolution table + dynamic bitrate percentages/bounds + separate FPS tiers**.

- Resolution table: 1920x1080 -> 1600x900 -> 1280x720 (`next_lower_resolution`, lines 1345-1361).
- FPS table: 60 -> 45 -> 30 for downward emergency behavior (`next_lower_fps`, lines 1363-1369).
- Profile labels: inferred `F0`-`F5`, not stored as a ladder index (`QualityProfile::profile_name`, lines 147-163).
- Bitrate bounds: inferred from the current profile label (`bitrate_bounds`, lines 165-174).
- Downward bitrate step: 8% or 12% reduction (`degrade`, lines 1000-1015).
- Upward bitrate step: 6% increase (`recover`, lines 1127-1145).
- `profile_generation` increments for resolution/FPS only, not bitrate (`apply_action`, lines 1151-1185).

### 7.6 Required R4 state behavior

Use an explicit adjacent quality index:

| Index | Resolution | FPS | Bitrate | Change from previous |
|---|---:|---:|---:|---|
| Q0 | 1920x1080 | 60 | 22 Mbps | nominal |
| Q1 | 1600x900 | 60 | 22 Mbps | resolution only |
| Q2 | 1280x720 | 60 | 22 Mbps | resolution only |
| Q3 | 1280x720 | 60 | 18 Mbps | bitrate only |
| Q4 | 1280x720 | 60 | 15 Mbps | bitrate only |
| Emergency E1 | 1280x720 | 45 | 15 Mbps | FPS only, separate state |
| Emergency E2 | 1280x720 | 30 | 15 Mbps | FPS only, separate state |

Normal downgrade must be `Q0 -> Q1 -> Q2 -> Q3 -> Q4`; recovery must be the exact reverse. Emergency recovery must restore FPS before entering Q4, then continue `Q4 -> ... -> Q0`. Every action advances one adjacent step only. Existing transition-active suppression remains mandatory.

## 8. Runtime differences: bitrate, resolution, and FPS

| Change | Pipeline control | Encoder rebuild | New session | Keyframe | Profile transition |
|---|---|---|---|---|---|
| Bitrate only, runtime API succeeds | `CapturePipelineControl::UpdateBitrate` | No | No | Yes | No |
| Bitrate only, runtime API fails | marks encoder rebuild/fallback | Yes | Yes | New stream starts with IDR | Yes |
| Resolution | prepare/restart pipeline | Yes | Yes | Yes | Yes |
| FPS | prepare/restart pipeline | Yes | Yes | Yes | Yes |

Evidence:

- Structural discriminator: `h264_send_probe.rs`, `adaptive_action_changes_video_structure`, lines 140-148.
- Runtime bitrate update: `capture_encode_probe.rs`, `CapturePipelineControl::UpdateBitrate`, lines 883-923.
- Sender control dispatch and runtime success/fallback handling: `h264_send_probe.rs`, lines 2841-2950.
- Structural transition: `h264_send_probe.rs`, `transition_profile`, lines 1242-1542 and invocation at 2951-3087.

### 8.1 Transition confirmation and rollback

The sender creates a monotonic transition sequence/generation, sends MPRF, waits for a matching MPAK, activates a fresh media session, waits for receiver readiness/settle, then commits. Receiver acceptance is pinned to the peer and old session and enforces monotonic sequence/generation. Duplicate controls are re-acknowledged without duplicate activation. Evidence: `profile_transition.rs:307-642`; `media_control.rs` profile control packet implementation and tests.

Finite deadlines are defined in `profile_transition.rs:9-16`, including control pending, first IDR, first render, settle, receiver hard, and sender total deadlines. A failed transition marks failure telemetry and leaves/returns the sender to a finite failed state rather than leaving a permanently active transition. The current controller also suppresses new adaptive actions while the transition is active (`adaptive_quality.rs:823-826`; sender begin/end transition calls in `h264_send_probe.rs`).

R4 should reuse this behavior unchanged. Q0/Q1/Q2 adjacent resolution steps each create a new session. Q2/Q3/Q4 bitrate steps normally stay in the same session; only the existing runtime-update failure fallback may create a new session.

## 9. Python/GUI and application-layer audit

### 9.1 Native preset source

`screen_runtime.py:38-69` is the authoritative application preset table. `native_screen_preset_info` at lines 92-110 normalizes unknown values back to `stable`.

`main_kivy.py:2139-2154` loads `gui_settings.json` and uses `DEFAULT_NATIVE_SCREEN_PRESET` when no key exists. `main_kivy.py:5810-5826` persists only the preset id (`screen_native_preset`), not width/height/FPS/bitrate scalars.

### 9.2 Native command construction

- Receiver: `_build_native_receiver_command`, `screen_runtime.py:807-834`, explicitly passes playout delay and repair from the selected preset.
- Sender: `_build_native_sender_command`, `screen_runtime.py:836-867`, explicitly passes width, height, FPS, and bitrate. It does not pass encoder, conversion backend, or adaptive mode, so Rust defaults control those fields.

This means changing only Rust defaults would not change GUI-selected resolution/FPS/bitrate, because the GUI always passes explicit values. Both layers must be aligned.

### 9.3 Config compatibility risk

Because the config stores a semantic preset id, changing the meaning of `stable` would silently change existing users on next launch. The R4 implementation should either:

1. add a preset schema/version and migrate old values explicitly; or
2. keep old labels as legacy choices and change only the default id to a new R4 preset.

This is an implementation compatibility decision, not a reason to revisit the fixed R4 default.

### 9.4 Help and documentation drift

- `README.md:10-12` repeats 20/50/80 Mbps presets and must be updated after behavior changes.
- `main.rs:2320-2341` lists accepted options and many probe defaults, but the `screen-send` help text does not state the current core width/height/FPS/bitrate/encoder/conversion/repair defaults. R4 should make the product defaults explicit.
- `screen_runtime.py` module header still describes an FFmpeg/ffplay runtime even though the module now owns Rust native process management; this is documentation debt, not an R4 runtime dependency.

## 10. PowerShell and validation-script classification

| Source | Values observed | Classification | R4 action |
|---|---|---|---|
| `scripts/package_native_media_test.ps1` | 1080p60, 50 Mbps, 250 ms, QSV auto, D3D11, NACK | Native media test package fixture | Do not treat as product default; update only if an R4 evidence package intentionally targets Q0 |
| `scripts/wgc_raii_validation/Run-Phase2.ps1` | repeated capture probe cases | WGC lifecycle validator | No change |
| `Run-Phase3.ps1` | CPU/D3D11, 720p/1080p, 30 FPS, 8 Mbps | loopback/resource validation fixture | No change |
| `Run-Phase4.ps1` | 720p30/8 Mbps/NACK/adaptive off shutdown cases | shutdown fixture | No change |
| `Run-AgoraLink-Stage6-Admin.ps1` | AppVerifier/GFlags execution matrix | verifier harness | No change |

Numbers such as 35 or 50 Mbps in scripts/logs are performance or validation parameters unless the application code imports them. No inspected product path imports these script values.

## 11. Test coverage audit

### 11.1 Adaptive behavior tests

| Test | File / line | Current assertion | R4 status / change |
|---|---|---|---|
| `adaptive_off_never_changes_fixed_profile` | `adaptive_quality.rs:1686` | off mode produces no changes | Keep unchanged |
| `fps_priority_and_hysteresis` | `adaptive_quality.rs:1509` | mild/network pressure lowers bitrate before resolution/FPS | **Replace/update**; contradicts Q0->Q1 resolution-first |
| `resolution_before_emergency_fps` | `adaptive_quality.rs:1535` | resolution before emergency FPS | Keep concept; update exact Q fixtures |
| `emergency_fps_reduction_requires_sustained_severe_pressure_at_floor` | `adaptive_quality.rs:1555` | 720p60 floor eventually becomes 45 FPS | Keep; change floor fixture to Q4=15 Mbps |
| `recovery_order_is_fps_resolution_bitrate` | `adaptive_quality.rs:1583` | FPS, then resolution, then bitrate | **Replace/update**; normal R4 reverse is bitrate, then resolution; emergency FPS remains separate |
| `spikes_stale_feedback_and_short_stability_do_not_change_profile` | `adaptive_quality.rs:1639` | hysteresis/stale guard | Keep; update fixture profile |
| `initial_decoder_warmup_then_sixty_fps_stays_at_f0_for_sixty_seconds` | `adaptive_quality.rs:1789` | startup does not falsely degrade | Keep; rename F0/Q0 semantics |
| `stable_network_at_fifty_eight_to_sixty_fps_keeps_f0` | `adaptive_quality.rs:1817` | stable profile remains nominal | Keep; change F0 to Q0 |
| `bitrate_only_action_does_not_increment_profile_generation` | `adaptive_quality.rs:1848` | bitrate-only does not increment structural generation | Keep |
| `transition_feedback_never_enters_classifier_or_triggers_positive_feedback` | `adaptive_quality.rs:1881` | transition feedback cannot trigger action | Keep unchanged |
| `virtual_soak_does_not_oscillate` | `adaptive_quality.rs:1729` | bounded changes under virtual soak | Keep; assert adjacent Q steps |
| `deterministic_ten_minute_soak_has_no_self_excited_degradation` | `adaptive_quality.rs:1922` | stable deterministic soak | Keep |
| `sliding_window_resets_at_session_boundary_and_requires_a_fresh_baseline` | `adaptive_quality.rs:1984` | session reset clears classifier window | Keep unchanged |
| `isolated_thirty_three_ms_present_sample_is_not_network_pressure` | `adaptive_quality.rs:2032` | isolated present jitter is not network pressure | Keep unchanged |
| `transport_window_gate_blocks_adaptation_even_after_local_warmup` | `adaptive_quality.rs:2043` | no action before transport baseline | Keep unchanged |
| `bottleneck_classification_matrix` | `adaptive_quality.rs:1711` | pressure dimensions classify independently | Keep unchanged; R4 does not change formulas |

### 11.2 Bitrate and default tests

| Test / self-test | File / line | Current assertion | Gap |
|---|---|---|---|
| explicit bitrate precedence | `bitrate.rs:138-164` | explicit 24 overrides BPF | Keep; promote to unit test or retain self-test |
| screen-send UDP/adaptive/audio parser checks | `main.rs:2657-2705` | payload/adaptive/audio parsing | Extend with exact R4 screen-send default assertion |
| receiver playout defaults | `main.rs:2724,2777` | 120 ms defaults | Keep unchanged |
| bitrate structural classification | `h264_send_probe.rs:4018` | bitrate is non-structural; resolution is structural | Keep |

Coverage gaps:

1. No unit test asserts the complete product default tuple (1080p60/22/Intel-QSV/D3D11/NACK/adaptive-off).
2. No test asserts exact Q0-Q4 values and adjacent/no-skip transitions.
3. No test asserts the full downgrade sequence and exact reverse restore sequence.
4. No test defines explicit `--bitrate-mbps` behavior when adaptive smoothness is enabled.
5. No GUI-level test asserts command construction for the R4 default preset.

### 11.3 Transition/session tests (retain unchanged)

`profile_transition.rs` includes:

- `all_control_packets_lost_never_activates_sender_session` (670)
- `duplicate_control_is_reacked_without_activation` (690)
- `higher_sequence_cannot_roll_generation_back` (712)
- `forged_peer_cannot_create_pending_transition` (739)
- `malformed_profile_datagrams_cannot_redirect_or_mutate_receiver_state` (754)
- `pending_expiry_keeps_old_session_active` (777)
- `expired_controls_cannot_replay_sequence_or_generation` (789)
- `matching_ack_is_required_before_sender_activation` (841)
- `all_acks_lost_leave_receiver_on_old_session_until_pending_expires` (854)
- `successful_transition_commits_each_side_once` (888)
- `delayed_new_session_activates_before_pending_deadline` (922)
- `readiness_timeout_is_finite_and_fails_after_ack` (948)
- `old_data_during_pending_and_stale_data_after_commit_do_not_switch_sessions` (963)
- `duplicate_ack_and_cancellation_are_idempotent_and_finite` (986)
- `cancellation_during_control_ack_and_readiness_is_finite` (1004)
- `damaged_settle_recovers_once_and_commits_within_hard_deadline` (1028)
- `repeated_damaged_settle_fails_after_one_restart` (1068)
- `cancellation_soak_leaves_no_sender_transition_active` (1093)
- `readiness_failure_telemetry_is_committed_before_snapshot` (1128)
- `matching_rejected_ack_terminates_control_wait_without_activation` (1149)
- `virtual_thirty_minute_fault_soak_has_no_permanent_transition_or_rollback` (1172)

These tests cover session activation, duplicate suppression, timeout, cancellation, rollback/failure convergence, and recovery settle. R4 should exercise the same suite without altering its assertions.

### 11.4 STREAM_CLOSE, window close, Ctrl+C, and worker join tests

- `media_control.rs`: stream close exact match, duplicate re-ack, invalid feedback/liveness separation (lines 961-1036).
- `h264_recv_view.rs`: receiver close retry, wrong close id, media datagram before ACK, simultaneous close, connection-reset liveness tests (lines 3943-4141).
- `shutdown.rs`: first-writer stop reason, idempotent cleanup, finite joins, failed cleanup behavior, socket release, shared cancellation, per-worker join budget, retained worker reap, Ctrl+C vs window close, monotonic runtime event context (lines 587-792).
- `callback_lifecycle.rs` and `wgc_latest_capture.rs`: callback drain ordering, idempotent close, startup cancellation, and WGC worker join (lines 240-378 and 688-710).

These are strong R3 regression anchors and are explicitly outside the R4 behavior-change scope.

## 12. R4 impact classification

### A. Must change

| File / symbol | Current behavior | Target behavior | Reason / tests | Regression risk |
|---|---|---|---|---|
| `rust-native/agoralink_media/src/main.rs`, `parse_screen_send_args` | 720p30/4, auto/auto, repair off | 1080p60/22, Intel-QSV, D3D11, NACK, adaptive off; explicit bitrate wins | Add complete default and precedence tests | CLI compatibility; capability fallback semantics |
| `rust-native/agoralink_media/src/adaptive_quality.rs`, `QualityProfile`, `degrade`, `recover`, tests | inferred F profiles, percentage bitrate, conditional order | explicit adjacent Q0-Q4 ladder plus separate emergency FPS, exact reverse recovery | Replace conflicting tests; add ladder/no-skip/soak tests | adaptive-only behavior; transition frequency |
| `screen_runtime.py`, `DEFAULT_NATIVE_SCREEN_PRESET`, `NATIVE_SCREEN_PRESETS`, command builders | GUI defaults to stable 720p30/20/off | default GUI command resolves to R4 tuple | command-construction tests/manual inspection | saved preset semantics |
| `main_kivy.py`, preset default/config handling | missing key -> `stable`; stores only semantic id | select/migrate an R4 default without silent legacy reinterpretation | config migration test/manual check | existing users' saved choice |
| `rust-native/agoralink_media/src/main.rs`, `print_help` | screen-send core defaults are incomplete/old | state R4 fixed defaults and explicit override | help snapshot/manual check | documentation only |

### B. May change

| File / symbol | Why it may be needed | Constraint |
|---|---|---|
| `h264_send_probe.rs`, adaptive telemetry/profile labels | expose Q0-Q4 and emergency state names in JSON | do not alter transition, NACK, capture, or send paths |
| `screen_share_presenter.py` | display updated preset label/profile | UI text only |
| `README.md` / `CHANGELOG.md` | remove 20/50/80 default drift and document 22 Mbps | change after implementation, not before tests |
| `scripts/package_native_media_test.ps1` | only if it becomes the official R4 evidence recipe | retain fixtures otherwise; do not treat as runtime source |
| `bitrate.rs` | likely no logic change; add a unit test for explicit precedence | keep explicit > BPF > default behavior |

### C. Explicitly do not change in R4

| Area | Files / evidence | Reason |
|---|---|---|
| WGC RAII and callback lifecycle | `wgc_latest_capture.rs`, `gpu_nv12_capture.rs`, `callback_lifecycle.rs` | validated R3 lifecycle; no ladder dependency |
| QSV/WMF encoder internals | `wmf_h264_encoder.rs`, `async_mft_wait.rs` | only selection default changes; encoder behavior is stable |
| D3D11 conversion | `gpu_nv12_capture.rs`, `capture_encode_probe.rs` conversion implementation | stable and measured; no ladder formula dependency |
| WMF decoder/renderers | `wmf_h264_decoder.rs`, `video_renderer.rs`, D3D11/GDI render modules | receiver media path not changed by policy |
| NACK and repair parameters | `repair.rs`, `h264_reassembly.rs`, sender/receiver NACK loops | R4 may select NACK as default but must not retune protocol/parameters |
| Playout strategy/delay | `playout_buffer.rs`, receiver playout code | current ~424 ms issue explicitly deferred |
| MPRF/MPAK and session rollover | `media_control.rs`, `profile_transition.rs` | R4 reuses established structural transition |
| STREAM_CLOSE | `media_control.rs`, sender/receiver close loops | validated shutdown protocol |
| Shutdown coordinator / worker join | `shutdown.rs` | validated finite cancellation and cleanup |
| Network pressure formulas | `adaptive_quality.rs:1273-1343` | fixed R4 scope says policy ladder only |

Selecting `RepairMode::Nack` as a product default is a CLI/application-layer default change, not a change to NACK mechanics or parameters.

### D. Defer

- Large `main.rs` split.
- Legacy AGM1 synthetic sender/receiver extraction.
- Playout-delay reduction or jitter-buffer redesign.
- New network classifier or pressure formula.
- FPS policy redesign beyond the fixed emergency 45/30 tiers.
- Wire-format or profile-control field extension.
- QSV, D3D11, WGC, decoder, renderer, audio, or A/V sync refactors.
- Clippy cleanup should be a separate baseline maintenance change, not mixed into R4 behavior commits.

## 13. `main.rs` split recommendation

Final recommendation: **C. Do not split for R4; refactor separately after R4.**

| Candidate module | Movable content | Dependency/cycle risk | Public API impact | Before R4? |
|---|---|---|---|---|
| `cli.rs` | `Command`, parsers, validation, help | high fan-out to all config enums; no cycle if carefully designed | internal only | No; best post-R4 target |
| `screen_send.rs` | only parser wrapper from `main`; runtime is already `h264_send_probe` | would create redundant naming/ownership now | internal | No |
| `screen_recv.rs` | only parser wrapper; runtime already `h264_recv_view` | redundant wrapper | internal | No |
| `runtime.rs` | dispatch/error policy | could become a thin facade but little R4 value | internal | No |
| `shutdown.rs` | already exists and owns cancellation/join logic | moving more entry code risks R3 cleanup behavior | existing internal API | No |
| `transition.rs` | already exists as `profile_transition.rs` and `media_control.rs` | merging/splitting risks protocol invariants | internal protocol API | No |
| `telemetry.rs` | JSON formatting currently distributed by subsystem | broad cross-module refactor | telemetry schema risk | Defer |
| `network.rs` | legacy UDP helpers plus production socket helpers | could conflate legacy and production paths | packet API risk | Defer |

R4 edits in `main.rs` are localized to one parser/default block, help text, and self-test. A split first would increase review surface without reducing policy risk.

## 14. Risks and open implementation questions

### Risks

1. **Conflicting defaults**: Rust CLI, GUI preset, README, and validation scripts currently carry different numbers.
2. **22 Mbps vs current adaptive bounds**: F1 floor 34 and F3 floor 20 make telemetry and exact Q3/Q4 impossible.
3. **Saved preset semantic drift**: changing `stable` in place silently changes existing users.
4. **Auto vs fixed backend semantics**: `auto` permits software/CPU fallback, while the fixed target names Intel QSV and D3D11.
5. **Bitrate fallback transition**: bitrate is normally in-place but can become structural when runtime update fails; R4 tests must allow this existing fallback without redefining the ladder.
6. **Dirty baseline**: many R3 modules are untracked relative to HEAD; an R4 branch/commit must first capture the intended source baseline deliberately.
7. **Strict Clippy gate**: approximately 70 diagnostics currently fail `-D warnings`; unrelated cleanup could obscure R4 review.
8. **Incomplete help**: users cannot infer the current standalone default tuple from `--help`.
9. **Frozen artifact availability**: historical evidence and hash exist, but a separately retained frozen EXE was not found under `_local_artifacts`.

### Open implementation questions

1. When `--adaptive-quality smoothness` and explicit `--bitrate-mbps` are both supplied, does the explicit value define only the initial/nominal Q0 bitrate, or should the fixed Q0-Q4 bitrate values remain 22/18/15? The explicit value must win in fixed mode; adaptive anchoring needs one documented rule.
2. Should Intel QSV/D3D11 defaults be hard requirements (`intel-qsv`, `d3d11`, fail if unavailable) or preferred defaults with current `auto` fallback? The product tuple is fixed; failure behavior remains to be specified.
3. Should the GUI introduce a new `r4_default` preset id or migrate `stable`? A versioned migration is safer than silently changing an existing semantic id.
4. Should old 50/80 Mbps GUI presets remain optional user choices after the 22 Mbps default is introduced? They are not blockers to making 22 the default, but labels must not imply product recommendation unless intended.
5. Will the release gate require strict Clippy immediately? If yes, schedule a separate lint-baseline commit before the R4 behavior series.

## 15. Recommended R4 commit plan

### Commit 1: tests describe the new policy, no product behavior change

- Scope: `adaptive_quality.rs` tests, `main.rs` self-test/default tests, optional Python command-construction test location.
- Add exact Q0-Q4 constants/table assertions, adjacent downgrade/restore, no skip, transition-active suppression, emergency separation, complete CLI default tuple, and explicit bitrate precedence.
- Existing tests `fps_priority_and_hysteresis` and `recovery_order_is_fps_resolution_bitrate` should be replaced or renamed to express R4 behavior.
- Expected state: selected new tests fail against old behavior; all unrelated R3 tests stay green.
- Rollback: remove tests only.
- Must not include: refactors, lint cleanup, protocol changes.

### Commit 2: unify fixed product defaults

- Scope: `main.rs` screen-send defaults and parser self-test.
- Behavior: 1080p60/22, Intel-QSV, D3D11, NACK, adaptive off; explicit `--bitrate-mbps` retains precedence.
- Tests: default tuple and explicit override.
- Rollback: restore constants/default arguments.
- Must not include: adaptive ladder or media runtime internals.

### Commit 3: introduce explicit adaptive ladder

- Scope: `adaptive_quality.rs` profile representation and telemetry names.
- Behavior: explicit Q0-Q4 plus separate E1/E2 emergency states; remove percentage/bounds as action generation for R4 mode.
- Tests: exact table, boundary identities, no-skip helper behavior.
- Rollback: restore old profile generator behind one patch.
- Must not include: transition/session code changes.

### Commit 4: implement downgrade and reverse recovery order

- Scope: `adaptive_quality.rs` `degrade`, `recover`, action application, related tests; optional telemetry-only integration in `h264_send_probe.rs`.
- Behavior: resolution-first through Q2, bitrate Q3/Q4, emergency FPS only after Q4, exact reverse restore, one action at a time.
- Tests: mild/severe hysteresis, cooldown, transition suppression, deterministic soak, no skipping.
- Rollback: revert controller policy while leaving table/test helpers isolated.
- Must not include: classifier thresholds, NACK, playout, transition wire/session logic.

### Commit 5: align GUI, preset, and help

- Scope: `screen_runtime.py`, localized `main_kivy.py` preset migration/default selection, `main.rs` help, `README.md`/`CHANGELOG.md`, optional presenter labels.
- Behavior: new users select R4 product tuple; old saved settings follow an explicit migration rule.
- Tests: native sender/receiver command arrays and config migration; Python `py_compile`.
- Rollback: restore preset table/default id without touching Rust policy.
- Must not include: layout redesign or unrelated UI cleanup.

### Commit 6: targeted regression and evidence

- Scope: tests/evidence only.
- Run 108+ Rust tests, strict validation gate, self-test, fixed 22 Mbps loopback/LAN evidence as separately authorized.
- Confirm WGC, transition, shutdown, STREAM_CLOSE, NACK, and playout regressions are absent.
- Rollback: evidence files only.
- Must not include: opportunistic code fixes.

## 16. Verification results from this audit

| Command | Result | Details |
|---|---|---|
| `cargo fmt --check` | PASS | no formatting differences |
| `cargo clippy --release --all-targets -- -D warnings` | FAIL | normal binary reports 68 errors; test target reports 70; categories include derivable impls, too many arguments, manual `is_multiple_of`, item order, and style diagnostics |
| `cargo test --release` | PASS | 108 passed, 0 failed, 0 ignored |
| `cargo build --release` | PASS | release target completed |
| `target/release/agoralink_media.exe self-test` | PASS | `{"type":"SELF_TEST","ok":true,"packet_format":"AGM1"}` |
| executable SHA-256 | MATCH | `55AA6B837D1CA2DFCF6362D8BEE3CFA5A9998DC8F769FD76A415DBE02DB44B05` |

No WGC, LAN, WASAPI, QSV runtime, packaging, or GUI test was executed or inferred by this audit. Historical R3 evidence is cited only as historical evidence.

## 17. Direct R4 implementation brief

1. Capture the intended dirty R3 source baseline in a deliberate branch/commit before R4; do not reconstruct it from HEAD.
2. Add tests for the fixed tuple and explicit Q0-Q4/E1/E2 ladder.
3. Change standalone `screen-send` defaults to 1080p60/22/Intel-QSV/D3D11/NACK/adaptive-off while preserving explicit bitrate precedence.
4. Replace current percentage/bounds action generation with one adjacent ladder index; do not change classifier inputs or thresholds.
5. Keep bitrate updates in place, structural resolution/FPS transitions on the existing MPRF/MPAK/new-session path, and transition-active suppression unchanged.
6. Align GUI default/preset and help with an explicit saved-config migration rule.
7. Run all R3 transition/shutdown/WGC/STREAM_CLOSE tests unchanged plus new R4 policy tests.
8. Resolve or formally baseline strict Clippy debt separately before declaring the R4 release gate green.

Recommended R4 approach: **Make a narrow, test-first policy/default change; preserve the R3 media and transition runtime; defer `main.rs` restructuring.**
