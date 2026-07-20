# AgoraLink v0.0.11 - R4 Screen Sharing Update

AgoraLink v0.0.11 promotes the validated R4 native screen-sharing runtime and its resolution-first quality policy.

## Highlights

- Default native screen sharing is now 1920x1080 at 60 FPS and 22 Mbps.
- NACK repair is enabled by default.
- Adaptive quality remains off by default and is opt-in.
- `auto` prefers Intel QSV and D3D11 when available, with the existing software and CPU fallbacks retained.
- The GUI adds an explicit `r4_default` preset; existing saved legacy presets remain compatible and keep their prior values.

## Adaptive Quality Order

```text
Q0  1920x1080 / 60 FPS / 22 Mbps
 -> Q1  1600x900 / 60 FPS / 22 Mbps
 -> Q2  1280x720 / 60 FPS / 22 Mbps
 -> Q3  1280x720 / 60 FPS / 18 Mbps
 -> Q4  1280x720 / 60 FPS / 15 Mbps
 -> E1  1280x720 / 45 FPS / 15 Mbps
 -> E2  1280x720 / 30 FPS / 15 Mbps
```

Recovery follows the exact reverse adjacent path. FPS is reduced only after sustained emergency pressure at Q4.

## Validation

- Rust release suite: 121 tests passed.
- Locked/offline independent release build and native self-test: passed.
- Python/GUI syntax, R4 command construction, preset, and runtime self-tests: passed.
- Command-line dual-host default profile, explicit 30 Mbps override, 22 Mbps 30-minute run, resolution-first adaptation, STREAM_CLOSE, and worker cleanup: passed according to retained R4 runtime evidence.
- Portable extraction, bundled native identity check, native self-test, and independent GUI startup/graceful close: passed.
- New crash dumps: none.
- Residual AgoraLink/native media processes: none.
- GUI real dual-host validation: **USER VALIDATION PENDING**.
- GUI append-file validation: **USER VALIDATION PENDING**.

## Known Non-Blocking Items

- The playout buffer has additional optimization headroom.
- QSV dynamic bitrate changes may fall back to encoder reconstruction.
- The real-network recovery path was not forced in this release pass.
- Windows MSVC `release + debuginfo` output is not yet byte-for-byte reproducible. The released portable is the retained, previously validated frozen artifact; structural comparison found identical `.text`, imports, and core PE layout.

## Download

- `AgoraLink_R4_portable_20260720.zip`
- SHA-256: `41CBAEE0ADC3F0F542811D783A7B84B66F1A011C56FD4F82D39FAEDD8B18E942`
