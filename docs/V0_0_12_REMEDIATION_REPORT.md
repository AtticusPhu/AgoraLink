# AgoraLink v0.0.12 Remediation Report

Date: 2026-07-20

Baseline: `c0e7bc5`

Branch: `audit-fixes-v0.0.12`
Scope: v0.0.11 audit follow-up, native-only media runtime, deterministic validation, and portable hardening.

## Result

`V0_0_12_REMEDIATION_COMPLETE`

The requested source changes are complete. Deterministic Rust/Python/PowerShell gates and a full portable dry-run passed. Hardware-dependent and dual-host gates are explicitly `NOT_RUN`; this report does not claim WGC, QSV, D3D11 window, or LAN results.

## Batch Results

| Batch | Commit | Result | Scope |
|---|---|---|---|
| 1 | `eab775b` | PASS | Control validation, reassembly budgets, short datagrams |
| 2 | `775549d` | PASS | Native-only runtime/UI/config/build migration |
| 3 | `f00f6c1` | PASS | Local stop channel and finite GUI process shutdown |
| 4 | `b3783c1` | PASS | Deterministic Python discovery and regression tests |
| 5 | `4915c40` | PASS | Locked environments and Windows CI definitions |
| 6 | `5817853` | PASS | PDB separation, privacy gate, portable/symbol scripts |
| 7 | `6655d75` | PASS | README, changelog, release and evidence reports |

## Input Boundary Hardening

### STREAM_CLOSE

- A receiver without a pinned peer and nonzero current video session ignores close packets.
- Accepted closes must match the pinned source and current nonzero session, carry a nonzero close ID, use a legal reason, and decode at the exact legal length.
- Replay close IDs are idempotent and do not retrigger shutdown.
- Rejected-close counters are aggregated instead of producing per-packet log noise.

### Reassembly

- `MAX_VIDEO_FRAME_BYTES`, `MAX_VIDEO_PACKET_COUNT`, aggregate slot/payload limits, and active-frame limits are enforced before allocating packet slots.
- Frame completion, expiry, rejection, session reset, and shutdown release charged budgets.
- Duplicate packets do not double-charge resources.
- FEC data-count metadata is constrained by the same packet-count and frame-budget rules.

### Datagram Length

- Dispatch and decode operate on `&datagram[..received_length]`.
- Buffers shorter than four bytes cannot be classified from stale bytes left by a previous receive.

## Native-Only Media Runtime

- Removed the external runtime capability module and external process/discovery branches.
- Removed the single-choice backend UI and external capability/path diagnostics.
- Added a one-time legacy configuration migration to `backend=native`, removing obsolete path/source keys.
- Packaging includes exactly one `agoralink_media.exe` and no external media bundle.
- Native unavailability is reported as a screen-sharing product error and does not stop chat/file transfer startup.

## Graceful Shutdown

- Rust screen modes accept `{"type":"LOCAL_STOP","reason":"gui_stop","version":1}` on stdin.
- Python uses an idempotent state path: running, stop requested, native stopping, native stopped, idle.
- The finite escalation order is graceful request, terminate, process-tree force-kill.
- Stdout/stderr reader threads, final JSON events, exit code, and shutdown telemetry are collected.
- Starting a new process while the previous runtime is stopping is rejected.

## Deterministic Engineering Gates

- Python test command is fixed to `unittest discover -s tests -p "test_*.py"`.
- A discovery count guard rejects zero tests.
- Runtime and build locks use exact versions.
- `rust-toolchain.toml` pins Rust 1.96.0, minimal profile, rustfmt/clippy, and MSVC x64 target.
- Windows CI defines Rust static, Python core, and PowerShell/schema jobs without claiming hardware tests.

## Portable and Symbols

- Public portable contains no PDB.
- Privacy checks run before compression and after independent extraction.
- Output cleanup is marker-guarded and restricted to a child of `_local_artifacts`.
- Explicit Python selection fails closed instead of silently using another interpreter.
- The optional symbols build was intentionally blocked: the PDB contains local checkout/toolchain paths. No public symbols archive was created.

## Preserved Product Contracts

- R4 default profile and adaptive ladder semantics.
- AGM1 video, DATA/FEC/NACK, MPRF/MPAK, and STREAM_CLOSE/ACK wire formats.
- WGC/QSV/WMF/D3D11 implementation internals outside the narrowly audited bounds/lifecycle changes.
- Chat, discovery, database, file-transfer protocol, resume, and append behavior.

## Deferred Work

- Large Rust/Python module split: deferred to a separate architecture branch.
- Repository license: `USER_DECISION_REQUIRED`; no license was selected by automation.
- Branch protection: not applied because this branch was not pushed.
- Dual-host, WGC, QSV, D3D11 window, and live GUI smoke: `NOT_RUN`.
