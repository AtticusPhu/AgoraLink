# AgoraLink v0.0.12 Remediation Report

Date: 2026-07-21

Baseline: `c0e7bc5` (`v0.0.11`)

Branch: `audit-fixes-v0.0.12`

Final source commit: `ba4369dfffc0d3efdba622050586f39c0c16404d`

Pull request: https://github.com/AtticusPhu/AgoraLink/pull/2

## Result

`V0_0_12_REMEDIATION_COMPLETE`

The requested source remediation is complete and the deterministic local and GitHub CI gates pass. Release readiness remains `BLOCKED` because the real GUI active-share stop matrix, dual-host GUI matrix, and live append-file smoke test require manual execution and have not been run.

## Change Batches

| Commit | Result | Scope |
|---|---|---|
| `eab775b` | PASS | STREAM_CLOSE validation, reassembly budgets, short datagrams |
| `775549d` | PASS | Native-only runtime/UI/config/build migration |
| `f00f6c1` | PASS | Local stop channel and finite GUI process shutdown |
| `b3783c1` | PASS | Deterministic Python discovery and regression tests |
| `4915c40` | PASS | Locked environments and Windows CI definition |
| `5817853` | PASS | PDB separation, privacy gate, portable/symbol scripts |
| `6655d75`, `a54e38c` | PASS | Initial reports and asset identity evidence |
| `163020b`, `32cd07a` | PASS | Exclude unused Kivy media providers and record evidence |
| `6f5af0f` | PASS | Structured native self-test version/capability metadata |
| `1917a2b` | PASS | Single-execution validation runner and deterministic logging |
| `ba4369d` | PASS | Remove CI timing race from close-retry validation |

## Input Boundary Hardening

- STREAM_CLOSE is accepted only from the pinned peer for the current nonzero session with an exact legal payload, close ID, and reason.
- Pre-session, zero-session, foreign-peer, stale-session, malformed, and replayed closes are rejected or handled idempotently.
- Packet count, frame bytes, aggregate slot/payload, and active-frame limits are enforced before allocation.
- Completion, expiry, rejection, reset, and shutdown release reassembly budgets; duplicate packets do not double-charge them.
- Datagram dispatch always uses the actual received length, preventing stale bytes in a reused buffer from affecting classification.

## Native-Only Runtime

- Production UI, command construction, capability discovery, process lifecycle, and packaging now use the bundled `agoralink_media.exe` only.
- Legacy external-backend settings migrate once to `backend=native`; obsolete executable/path keys are removed.
- Native capability failures remain scoped to screen sharing and do not prevent chat or file-transfer startup.
- AGM1, DATA/FEC/NACK, MPRF/MPAK, STREAM_CLOSE/ACK, chat, discovery, database, and file-transfer contracts were preserved.

## Graceful Shutdown

- Native screen modes accept the versioned `LOCAL_STOP` command on stdin.
- Python follows a bounded sequence: graceful request, wait for `NATIVE_SCREEN_STOPPED`, terminate, then process-tree force kill only as the last resort.
- Stop is idempotent, app close reuses the same runtime path, starts are rejected while stopping, and stdout/stderr reader threads are joined.
- A local runtime harness completed 3/3 real native receiver cycles with `NATIVE_SCREEN_STOPPED`, exit code 0, no terminate/kill, and no residual process. This is supporting runtime evidence, not a substitute for the pending interactive GUI active-share test.

## Deterministic Validation

- Native `self-test` emits JSON containing `type`, `ok`, `version`, and capability fields.
- `ValidationRunner.psm1` executes each command once, keeps stdout/stderr separate, records structured metadata, handles Unicode/space/apostrophe paths, and kills timed-out process trees.
- Python validation rejects zero discovered tests.
- The close-handshake retry test now waits for an observed retry datagram before returning ACK instead of relying on `sleep(35ms)`. It passed 20 consecutive targeted runs and the full debug/release suites.
- GitHub Actions run `29795347977` passed Rust, Python, and PowerShell Windows jobs.

## Portable Hardening

- Public portable output contains no PDB, DMP, source, test data, or removed external-media binaries.
- Optional Kivy video, ffpyplayer, and GStreamer providers are excluded and rejected by the final scan.
- Rust path remapping and staging/extraction privacy scans prevent current user/source paths from entering the public package.
- The PDB privacy gate remains fail closed; no public symbols archive was created.

## Deferred and Manual Gates

- Real GUI active-share stop/restart: `MANUAL_REQUIRED`.
- Dual-host GUI start/sender stop/restart/receiver stop: `MANUAL_REQUIRED`.
- Live append-file transfer while the first transfer is active: `MANUAL_REQUIRED`.
- WGC/QSV/D3D11 and LAN quality claims: not made by this remediation.
- Repository license selection: `USER_DECISION_REQUIRED`.

## Conclusion

Source remediation and automated validation are complete. The branch is pushed, PR #2 is mergeable, all configured checks pass, and source commit `ba4369d` is resolvable on GitHub. The release gate remains `BLOCKED` until the three manual product matrices are completed.
