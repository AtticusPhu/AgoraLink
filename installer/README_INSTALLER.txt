AgoraLink installer notes

Default install path:
%LOCALAPPDATA%\Programs\AgoraLink

Shortcuts:
- Start Menu\AgoraLink\AgoraLink.lnk
- Start Menu\AgoraLink\Uninstall AgoraLink.lnk
- Desktop\AgoraLink.lnk, if the desktop shortcut component is selected

Network ports:
- UDP 9999 is used for AgoraLink chat and file transfer.
- UDP 50020, or another negotiated UDP port, is used for screen sharing.

Firewall:
AgoraLink does not silently change Windows Firewall rules during installation.
On first run, allow Windows Firewall access for private networks.
If manual firewall setup is needed, run allow_firewall_udp_9999_admin.bat as Administrator.

Bundled media tools:
The full package expects FFmpeg under:
_internal\tools\ffmpeg\bin
Native Lite does not include ffmpeg.exe, ffplay.exe, or ffprobe.exe.
The packaged app expects Rust native media under:
_internal\tools\agoralink_media\agoralink_media.exe
Native Lite screen sharing uses the Rust native video backend and is currently video-only.
System audio screen sharing requires the full package with FFmpeg backend.

Native Lite release build:
Use scripts\package_release_v0_0_10_native_lite.ps1 for AgoraLink Native Lite v0.0.10.
The script checks that app_paths.py APP_VERSION matches the package version, builds the Rust native media release binary, runs PyInstaller with AGORALINK_PACKAGE_FLAVOR=native_lite, removes bundled FFmpeg tools, verifies no ffmpeg.exe/ffplay.exe/ffprobe.exe remain, then creates the portable zip and NSIS installer.
If PyInstaller is not installed in the selected Python environment, pass -Python or -PyInstaller explicitly.

Uninstall behavior:
The uninstaller removes the installed program files and shortcuts.
It does not delete local user data by default:
%LOCALAPPDATA%\AgoraLink
