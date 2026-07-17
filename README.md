# AgoraLink

AgoraLink is a Windows-focused LAN productivity tool for chat, file transfer, and screen sharing.

## v0.0.11 Focus

- Compact tool-style UI with lightweight About and Diagnostics information.
- Rust native screen backend support for video-only screen sharing.
- Native screen presets:
  - Stable: 1280x720, 30fps, 20Mbps
  - Recommended: 1920x1080, 60fps, 50Mbps, 250ms playout, NACK repair
  - High Quality: 1920x1080, 60fps, 80Mbps, 300ms playout, NACK repair
- Diagnostic bundle export with app version, package flavor, screen capability, recent non-sensitive logs, and a safe config snapshot.

## Package Flavors

- Native Lite: Rust native video backend, no bundled FFmpeg, video-only screen sharing.
- Full: includes the FFmpeg backend when packaged with FFmpeg tools.
- Source: development checkout.

System audio screen sharing requires the Full package with the FFmpeg backend. Native Lite remains video-only.

## Diagnostics

Use Settings or the Diagnostics window to export a diagnostic bundle. The bundle is designed to avoid private keys, PINs, chat content, and transferred file contents.
