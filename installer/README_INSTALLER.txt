AgoraLink native installer notes
================================

The installer consumes the PyInstaller one-folder output under:

dist\AgoraLink

Required media runtime:

_internal\tools\agoralink_media\agoralink_media.exe

Build the Rust runtime in release mode before invoking PyInstaller. The package
script verifies the GUI executable and bundled native runtime before creating
the portable archive or NSIS installer.

Screen sharing uses automatically selected high UDP ports. Chat and file
transfer continue to use UDP 9999. Windows Firewall access may be required on
private networks.
