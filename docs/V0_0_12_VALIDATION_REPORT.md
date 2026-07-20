# AgoraLink v0.0.12 Validation Report

Date: 2026-07-20  
Branch: `audit-fixes-v0.0.12`  
Validation type: deterministic local gates plus portable dry-run.

## Status Summary

| Area | Status | Evidence |
|---|---|---|
| Rust formatting | PASS | `cargo fmt -- --check` |
| Rust check | PASS | locked/offline, one job |
| Rust debug tests | PASS | 141 passed, 0 failed |
| Rust release tests | PASS | 141 passed, 0 failed |
| Rust Clippy debug | PASS | required command completed |
| Rust Clippy release | PASS | required command completed |
| Rust documentation | PASS | locked/offline/no-deps |
| Rust release build | PASS | Rust 1.96.0 MSVC x64 |
| Rust self-test | PASS | `{"type":"SELF_TEST","ok":true}` |
| Python compileall | PASS | generated/build trees excluded |
| Python dependency check | PASS | no broken requirements |
| Python discovery guard | PASS | 30 tests discovered |
| Python tests | PASS | 30 passed, 0 failed |
| Python runtime self-test | PASS | 18/18 checks |
| PowerShell parse | PASS | 18 tracked scripts/modules |
| JSON parse | PASS | 5 tracked JSON files |
| Portable dry-run | PASS | package, scan, extract, scan, self-test |
| GitHub Actions execution | NOT_RUN | workflow added; branch not pushed |
| Real WGC/QSV/D3D11 | NOT_RUN | hardware/manual gate |
| Dual-host LAN | NOT_RUN | requires two real hosts |
| Live file-append smoke | NOT_RUN | deterministic append tests passed |

## Rust Evidence

Toolchain:

```text
rustc 1.96.0 (stable-x86_64-pc-windows-msvc)
```

Executed:

```powershell
cargo fmt -- --check
cargo check --locked --offline --jobs 1
cargo test --locked --offline --jobs 1
cargo test --release --locked --offline --jobs 1
cargo clippy --locked --offline --all-targets --all-features --jobs 1
cargo clippy --release --locked --offline --all-targets --all-features --jobs 1
cargo doc --locked --offline --no-deps --jobs 1
cargo build --release --locked --offline --jobs 1
cargo run --locked --offline -- self-test
```

Debug and release each ran 141 tests. The required Clippy commands completed. A separate strict `-D warnings` comparison reports 68 binary and 70 test diagnostics, exactly matching the existing R4 baseline (delta 0); the historical lint debt was not mixed into this remediation.

## Python Evidence

Build interpreter: official CPython 3.12.10 x64 in a fresh venv populated from `requirements.lock` and `build-requirements.lock`.

Executed:

```powershell
python -B -m compileall -q <tracked source tree>
python -m pip check
python -B scripts/check_python_test_count.py
python -B -m unittest discover -s tests -p "test_*.py" -v
python -B screen_runtime.py --self-test
```

Results: 30 discovered tests, 30 passed, dependency check clean, and all 18 runtime self-test checks true. Compilation excluded ignored venv, artifact, target, build, and dist trees so generated dependencies were not treated as project source.

## Regression Coverage Added

- Pre-session/zero/foreign/stale/invalid/replayed close handling.
- Maximum legal and over-limit packet counts.
- Aggregate slot/payload budgets and budget release.
- FEC packet-count consistency and short/reused datagrams.
- Legacy external backend/path migration and idempotence.
- Native-only command construction and portable path selection.
- Graceful stop first, stopped-event wait, double stop, app close, start/stop race, reader join, and bounded escalation.
- File hash/header, append queue independence, resume metadata, unusual Windows paths, and diagnostic export privacy.

## Portable Dry-Run

- Python: 3.12.10.
- ZIP size: 43,626,711 bytes.
- ZIP SHA-256: `60D4E040379F1DFA00894767A81BE924E3A7C2C33896CF75073DD71053DBF941`.
- PDB count: 0.
- Removed external-media file-name count: 0.
- Source-file count: 0.
- Pre-compression privacy scan: PASS.
- Independent extraction privacy scan: PASS.
- Bundled native self-test before and after compression: PASS.

This is dry-run evidence for Batch 6, not the final release asset. Final asset identity is written after the documentation commit and recorded in the portable report/build result.

## Privacy Notes

The portable scan found no current user profile or source checkout prefix. Eight prebuilt third-party DLL/PYD files contain their vendors' upstream build-machine paths; these are recorded as provenance and are not local AgoraLink source paths.

The PDB privacy scan failed on local checkout/toolchain paths. That is expected fail-closed behavior; no public symbols ZIP was produced.

## Process and Crash Checks

- Residual `agoralink_media`, removed-media, or viewer processes after deterministic tests: 0.
- Crash dump candidates created after the validation start: 0.
- No real capture/encoder/renderer session was claimed by these checks.

## Limitations

`NOT_RUN` is intentional for WGC, QSV, D3D11 window rendering, dual-host network behavior, peer stop, and live GUI smoke. Those require the manual Windows release matrix and cannot be inferred from unit tests or loopback-free self-tests.

