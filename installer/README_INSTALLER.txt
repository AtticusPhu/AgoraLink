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
The packaged app expects FFmpeg under:
_internal\tools\ffmpeg\bin
The packaged app expects Rust native media under:
_internal\tools\agoralink_media\agoralink_media.exe

Uninstall behavior:
The uninstaller removes the installed program files and shortcuts.
It does not delete local user data by default:
%LOCALAPPDATA%\AgoraLink
