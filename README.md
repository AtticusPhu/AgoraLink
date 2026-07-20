# AgoraLink

AgoraLink is a Windows-focused LAN productivity tool for chat, file transfer, and screen sharing.

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

## Diagnostics

Use Settings or the Diagnostics window to export a diagnostic bundle. The bundle is designed to avoid private keys, PINs, chat content, and transferred file contents.
