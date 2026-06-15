FFmpeg binaries for AgoraLink screen sharing.

Source:
Gyan.dev FFmpeg full build, installed via winget package Gyan.FFmpeg.

Bundled executables:
- tools/ffmpeg/bin/ffmpeg.exe
- tools/ffmpeg/bin/ffplay.exe

Purpose:
- ffmpeg.exe: screen capture and encoding
- ffplay.exe: low-latency screen stream playback

Note:
The tested Gyan full build runs as standalone executables in an isolated directory.
No extra DLL files were required in local testing.

Licensing:
The bundled FFmpeg build reports --enable-gpl and --enable-version3.
When redistributing installers or portable packages, keep FFmpeg license/source information with the release.
