# AgoraLink v0.0.12 Validation Report

Date: 2026-07-21

Branch: `audit-fixes-v0.0.12`

Source commit: `ba4369dfffc0d3efdba622050586f39c0c16404d`

PR: https://github.com/AtticusPhu/AgoraLink/pull/2

## Final Status

`V0_0_12_FINAL_VALIDATION_BLOCKED`

All deterministic local gates and configured GitHub checks pass. The release is not ready because real GUI active-share, dual-host GUI, and live append-file validation remain `MANUAL_REQUIRED`.

## Gate Summary

| Gate | Result | Detail |
|---|---|---|
| Rust fmt/check | PASS | locked/offline, one job |
| Rust debug tests | PASS | 142 passed, 0 failed |
| Rust release tests | PASS | 142 passed, 0 failed |
| Rust Clippy debug/release | PASS | all targets/features |
| Rust doc/build release | PASS | locked/offline |
| Native self-test | PASS | structured JSON with version and capabilities |
| Validation runner | PASS | single execution, split logs, bounded timeout |
| Cargo help contamination | PASS | not present |
| Python compile/dependencies | PASS | CPython 3.12.10, `pip check` clean |
| Python discovery/tests | PASS | 30 discovered, 30 passed |
| Screen runtime self-test | PASS | 18/18 checks |
| PowerShell parse | PASS | 22 tracked/unignored files |
| PSScriptAnalyzer | TOOL_NOT_INSTALLED | not installed automatically |
| GitHub Actions | PASS | three Windows jobs |
| Portable build/scans | PASS | native-only, staging and extraction |
| Portable independent GUI launch | PASS | real window, WM_CLOSE, exit 0, no force cleanup |
| Native runtime graceful stop harness | PASS | 3/3 receiver cycles, no terminate/kill |
| GUI active-share stop/restart | MANUAL_REQUIRED | interactive authenticated UI not exercised |
| Dual-host GUI matrix | MANUAL_REQUIRED | requires two real Windows hosts |
| Live append-file smoke | MANUAL_REQUIRED | requires two active application peers |

## Local Evidence

Primary complete run:

`_local_artifacts/V0_0_12_FINAL_VALIDATION/run_20260721_101111`

Results:

- 22 structured commands completed in 163,262ms.
- Rust debug: 142/142.
- Rust release: 142/142.
- Python: 30/30.
- Runtime self-test: 18/18.
- PowerShell parse: 22 files, zero parse errors.
- New crash dumps: 0.
- Residual `agoralink_media`, FFmpeg-family, or viewer processes: 0.

The first attempt at `run_20260721_101027` failed before code validation because the restricted execution sandbox could not spawn the venv's official Python base executable in `AppData`. The same script then ran outside that restriction and passed. This environment failure is retained as evidence and is not counted as a product failure.

Validation runner smoke:

`_local_artifacts/validation_runner_smoke/20260721_094738_268`

- Three commands recorded exactly once.
- Space, Chinese, and apostrophe arguments round-tripped correctly.
- Timeout returned 124 and left no child process.
- No cargo help output contaminated logs.

The close-retry test that failed once in CI used `sleep(35ms)` and was sensitive to Windows runner scheduling. Commit `ba4369d` makes the peer wait for and validate the second close datagram before ACK. The corrected test passed 20/20 targeted repetitions and both complete suites.

## GitHub Actions

Workflow run: https://github.com/AtticusPhu/AgoraLink/actions/runs/29795347977

- `rust-static-windows`: PASS (4m02s).
- `python-core-windows`: PASS (51s).
- `powershell-parse-windows`: PASS (45s).

PR #2 is open and mergeable. Commit `ba4369dfffc0d3efdba622050586f39c0c16404d` is resolvable on GitHub.

## Graceful Stop Evidence

Runtime evidence:

`_local_artifacts/V0_0_12_FINAL_VALIDATION/runtime_graceful_stop_v2`

Three real native `screen-recv` processes were started through `ScreenRuntime`, stopped with production reason `gui_stop`, emitted `NATIVE_SCREEN_STOPPED`, exited with code 0, joined reader threads, and required neither terminate nor force kill. Stop times were 32ms, 47ms, and 31ms.

Portable GUI launch evidence:

`_local_artifacts/V0_0_12_FINAL_VALIDATION/portable_gui_launch_ba4369d`

The extracted `AgoraLink.exe` created a real top-level window. The harness posted `WM_CLOSE`; the process exited with code 0 in 3,430ms, without forced cleanup or residual processes.

These checks validate process lifecycle and independent launch. They do not prove the pending sequence of logging in, starting an active screen share from the GUI, clicking Stop, and restarting it three times.

## Portable Candidate

- Path: `_local_artifacts/V0_0_12_RELEASE_FINAL_ba4369d/AgoraLink_v0.0.12_portable.zip`.
- Size: 41,881,098 bytes.
- SHA-256: `9927AE652064670E9CE771B4BA93D0F13B38BA33DDCE697E60F7ECFAB4CCF479`.
- Native SHA-256: `DFA7A6F4B4F3D4C71CD9F7B76E754F4D0C7EAB8D0F24963DCACCA8BE15A70A17`.
- BUILD_INFO source commit: `ba4369dfffc0d3efdba622050586f39c0c16404d`.
- ZIP entries: 1,447; files scanned: 1,434.
- FFmpeg/PDB/DMP/source counts: 0.
- Staging and extracted privacy scans: PASS.
- Native self-test before and after extraction: PASS.

## Manual Blockers

1. Authenticated GUI active-share stop/restart, 3/3 cycles.
2. Dual-host GUI sender stop, restart, receiver stop, and app-close behavior.
3. Append a second file while the first is actively transferring and verify independent IDs/cards/workers and both hashes.

No WGC, QSV, D3D11, WASAPI, LAN, or file-transfer results are inferred or fabricated. Until the three matrices above pass, release readiness is `BLOCKED`.
