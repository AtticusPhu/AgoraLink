!include "MUI2.nsh"

!define ROOT ".."
!define APP_NAME "AgoraLink"
!define APP_PUBLISHER "AgoraLink"
!define APP_EXE "AgoraLink.exe"
!define APP_VERSION "1.0.0"
!define FFMPEG_EXE "${ROOT}\dist\AgoraLink\_internal\tools\ffmpeg\bin\ffmpeg.exe"
!define FFPLAY_EXE "${ROOT}\dist\AgoraLink\_internal\tools\ffmpeg\bin\ffplay.exe"

Name "${APP_NAME}"
OutFile "..\dist\AgoraLink_Setup_v0.0.8.exe"
InstallDir "$LOCALAPPDATA\Programs\${APP_NAME}"
InstallDirRegKey HKCU "Software\${APP_NAME}" "InstallDir"
RequestExecutionLevel user

!define MUI_ABORTWARNING
!define MUI_ICON "${ROOT}\assets\app.ico"
!define MUI_UNICON "${ROOT}\assets\app.ico"
!define MUI_FINISHPAGE_TEXT "AgoraLink uses UDP 9999 for chat and file transfer. Screen sharing uses UDP 50020 or an automatically selected UDP port.$\r$\n$\r$\nOn first run, allow Windows Firewall access for private networks. AgoraLink does not modify firewall rules automatically. If needed, run allow_firewall_udp_9999_admin.bat as Administrator from the install folder."

!if /FILEEXISTS "${FFMPEG_EXE}"
!else
    !error "Missing bundled FFmpeg: ${FFMPEG_EXE}. Build PyInstaller output before running NSIS."
!endif
!if /FILEEXISTS "${FFPLAY_EXE}"
!else
    !error "Missing bundled FFplay: ${FFPLAY_EXE}. Build PyInstaller output before running NSIS."
!endif

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

Section "AgoraLink program files" SEC01
    SectionIn RO
    SetOutPath "$INSTDIR"
    File /r "${ROOT}\dist\AgoraLink\*.*"
    File "${ROOT}\allow_firewall_udp_9999_admin.bat"
    File "${ROOT}\installer\README_INSTALLER.txt"

    CreateDirectory "$SMPROGRAMS\${APP_NAME}"
    CreateShortcut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
    CreateShortcut "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk" "$INSTDIR\Uninstall.exe"

    WriteUninstaller "$INSTDIR\Uninstall.exe"
    WriteRegStr HKCU "Software\${APP_NAME}" "InstallDir" "$INSTDIR"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "DisplayName" "${APP_NAME}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "DisplayVersion" "${APP_VERSION}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "Publisher" "${APP_PUBLISHER}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "DisplayIcon" "$INSTDIR\${APP_EXE}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "UninstallString" "$INSTDIR\Uninstall.exe"
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "NoRepair" 1
SectionEnd

Section "Desktop shortcut" SEC_DESKTOP
    CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
SectionEnd

Section "Uninstall"
    Delete "$DESKTOP\${APP_NAME}.lnk"
    Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
    Delete "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk"
    RMDir "$SMPROGRAMS\${APP_NAME}"

    RMDir /r "$INSTDIR"

    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}"
    DeleteRegKey HKCU "Software\${APP_NAME}"
SectionEnd


