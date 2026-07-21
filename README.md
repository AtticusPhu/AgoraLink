# AgoraLink

AgoraLink is a Windows-focused LAN productivity tool for chat, file transfer, and screen sharing.

## Platform and Runtime

- Windows 10/11 x64.
- CPython 3.12 for source and build workflows.
- The portable package includes the Rust `agoralink_media.exe` runtime.
- Screen sharing uses only the bundled native runtime.
- Hardware encoding and D3D11 conversion are preferred when available, with the existing WMF/CPU native fallbacks retained.

## R4 Native Screen Policy

The default fixed Rust native screen profile is **1920x1080, 60 FPS, 22 Mbps, NACK repair, adaptive quality off**. The 22 Mbps value is a default, not a maximum. An explicit `--bitrate-mbps` value takes precedence over `--quality-bpf`, and `--quality-bpf` takes precedence over the 22 Mbps fallback.

When `--adaptive-quality smoothness` is enabled, the normal quality path is strictly adjacent:

```text
Q0  1920x1080 @ 60 FPS / B
Q1  1600x900  @ 60 FPS / B
Q2  1280x720  @ 60 FPS / B
Q3  1280x720  @ 60 FPS / min(B, 18 Mbps)
Q4  1280x720  @ 60 FPS / min(B, 15 Mbps)
```

Degradation follows `Q0 -> Q1 -> Q2 -> Q3 -> Q4`; recovery follows the exact reverse path. FPS reduction is reserved for sustained emergency pressure after Q4 (`E1` at 45 FPS, then `E2` at 30 FPS).

`--encoder auto` prefers Intel Quick Sync Video when available and retains the existing software fallback. `--convert-backend auto` prefers D3D11 and retains the existing CPU conversion fallback. These are preferences, not hardware guarantees.

## v0.0.12 Focus

- Compact tool-style UI with lightweight About and Diagnostics information.
- A single bundled Rust native media backend for screen sharing.
- Native screen presets:
  - R4 Default: 1920x1080, 60fps, 22Mbps, 250ms playout, NACK repair
  - Legacy stable: 1280x720, 30fps, 20Mbps
  - Legacy high-bandwidth: 1920x1080, 60fps, 50Mbps, 250ms playout, NACK repair
  - Experimental: 1920x1080, 60fps, 80Mbps, 300ms playout, NACK repair
- Diagnostic bundle export with app version, package flavor, screen capability, recent non-sensitive logs, and a safe config snapshot.

## Native Media Runtime

Screen sharing uses the bundled `agoralink_media.exe` runtime. Video follows
the WGC, D3D11, QSV/WMF, AGM1 UDP, WMF decode, and D3D11 render path. System
audio is enabled only when the native runtime self-test reports capture and
playback capability; otherwise screen sharing continues video-only.

If the native runtime or a required Windows capability is unavailable, AgoraLink fails the screen-share operation with a product error. Chat and file transfer remain available.

## Diagnostics

Use Settings or the Diagnostics window to export a diagnostic bundle. The bundle is designed to avoid private keys, PINs, chat content, and transferred file contents.

## Development Validation

Create a clean Python 3.12 environment and install the exact runtime and build locks:

```powershell
.\scripts\Setup-DevEnvironment.ps1 -IncludeBuildDependencies
```

The deterministic local gates are:

```powershell
python -B -m unittest discover -s tests -p "test_*.py" -v
python -B screen_runtime.py --self-test

cd rust-native\agoralink_media
cargo fmt -- --check
cargo check --locked --offline
cargo test --locked --offline
cargo test --release --locked --offline
cargo clippy --locked --offline --all-targets --all-features
cargo clippy --release --locked --offline --all-targets --all-features
```

Real WGC capture, hardware encoder, D3D11 window, and dual-host LAN checks remain manual Windows release gates.

## Portable Packaging

Build the v0.0.12 portable asset with:

```powershell
.\scripts\package_release_v0_0_12.ps1 -Python <locked-python-3.12>
```

The public portable excludes PDB files, dumps, logs, user configuration, source files, test data, secrets, removed media artifacts, and unused Kivy video/GStreamer providers. Symbols are handled by a separate privacy-gated script and are not published when local build paths remain in the PDB.

## License

The project owner has not selected a repository license for v0.0.12. License status is `USER_DECISION_REQUIRED`; no license text has been inferred or added by automation.
