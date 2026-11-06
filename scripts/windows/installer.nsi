; Установщик Screen Recorder 4 для Windows (NSIS 3, per-user).
!ifndef APP_VERSION
  !define APP_VERSION "4.0.0-alpha.2"
!endif
!ifndef PAYLOAD
  !define PAYLOAD "..\..\dist\windows"
!endif
!ifndef OUTDIR
  !define OUTDIR "..\..\dist"
!endif

!define APP_NAME "Screen Recorder"
!define APP_EXE "screenrecorder.exe"
!define ICON "..\..\assets\icon.ico"

Name "${APP_NAME} ${APP_VERSION}"
OutFile "${OUTDIR}\ScreenRecorder-${APP_VERSION}-win64-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\Screen Recorder"
InstallDirRegKey HKCU "Software\ScreenRecorder" "InstallDir"
RequestExecutionLevel user
Unicode true
SetCompressor /SOLID lzma
Icon "${ICON}"
UninstallIcon "${ICON}"
ShowInstDetails show
ShowUninstDetails show

Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Section "Установить Screen Recorder" SEC_MAIN
  SectionIn RO
  SetOutPath "$INSTDIR"
  File /r "${PAYLOAD}\*.*"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateDirectory "$SMPROGRAMS\Screen Recorder"
  CreateShortcut "$SMPROGRAMS\Screen Recorder\Screen Recorder.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\icon.ico"
  CreateShortcut "$DESKTOP\Screen Recorder.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\icon.ico"
  WriteRegStr HKCU "Software\ScreenRecorder" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScreenRecorder" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScreenRecorder" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScreenRecorder" "Publisher" "Freekazoid"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScreenRecorder" "DisplayIcon" "$INSTDIR\icon.ico"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScreenRecorder" "UninstallString" "$INSTDIR\uninstall.exe"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScreenRecorder" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScreenRecorder" "NoRepair" 1
SectionEnd

Section "Uninstall"
  Delete "$DESKTOP\Screen Recorder.lnk"
  Delete "$SMPROGRAMS\Screen Recorder\Screen Recorder.lnk"
  RMDir "$SMPROGRAMS\Screen Recorder"
  RMDir /r "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScreenRecorder"
  DeleteRegKey HKCU "Software\ScreenRecorder"
SectionEnd
