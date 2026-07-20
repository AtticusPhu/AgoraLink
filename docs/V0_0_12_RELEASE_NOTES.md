# AgoraLink v0.0.12 Release Notes

Release date: 2026-07-20

## Summary

v0.0.12 hardens the native media control/reassembly boundaries, removes the external media runtime path, adds finite graceful shutdown for GUI-managed native screen processes, and makes validation and portable packaging fail closed.

## Security and Robustness

- Rejects native screen close messages before peer/session establishment.
- Validates peer, session, close ID, reason, structure, and replay state before accepting a close.
- Bounds per-frame packet count and aggregate in-flight packet/payload allocation before constructing a frame assembly.
- Uses the actual `recv_from` length for packet classification and decoding.
- Preserves legal STREAM_CLOSE/ACK, NACK repair, AGM1 video, and profile transition behavior.

## Native Media

- Screen sharing now has one bundled native Windows path: WGC capture, D3D11 conversion, QSV/WMF H.264 encoding, AGM1 UDP, WMF decode, and D3D11 rendering.
- Legacy backend/path settings migrate once to `backend=native` and obsolete keys are removed.
- Native capability failures produce a product error; chat and file transfer continue to work.
- The R4 default remains 1920x1080, 60 FPS, 22 Mbps, NACK repair, and adaptive quality off.

## Process Lifecycle

- GUI stop first sends a local JSON stop request over stdin.
- The runtime waits for the native stopped event and process exit using finite deadlines.
- Terminate and process-tree force-kill remain bounded last-resort fallbacks.
- Reader threads and final JSON events are collected before the process state returns to idle.

## Validation and Packaging

- Python discovery uses an explicit test directory/pattern and fails when zero tests are found.
- Runtime and build dependencies are exactly pinned; Rust uses a repository toolchain file.
- Windows CI definitions cover static Rust, Python core, and PowerShell/schema checks.
- The public portable excludes PDBs, dumps, logs, user configuration, source, test data, secrets, and removed media artifacts.
- A separate symbols workflow refuses publication while local paths remain in the PDB.

## Known Release Gates

- Real WGC/QSV/D3D11 and dual-host LAN regression tests are not represented by CI and remain manual Windows gates.
- Live two-host file-append smoke was not run in this remediation workspace; deterministic file-transfer tests passed.
- A repository license has not been selected. Status: `USER_DECISION_REQUIRED`.
- The large Rust/Python module split is deferred to a dedicated architecture change after golden CLI/JSON/state-machine coverage is established.

