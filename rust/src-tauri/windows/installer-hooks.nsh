; Hooks into Tauri's NSIS installer (bundle.windows.nsis.installerHooks).
;
; The Rust app replaces the Electron app on the same PC. The Electron app's
; updater can install this installer itself (beta.yml / latest.yml point at
; it), so a PC moves over with no one there; these hooks make that clean.

; The Electron app's per-user install, as electron-builder registers it: the
; key is a UUID v5 of its appId (com.falcongreen.scalestation) in
; electron-builder's namespace.
!define ELECTRON_GUID "9db6922f-ec91-5f75-87ee-f961284c2b37"
!define ELECTRON_UNINSTALLER "Uninstall Falcon Scale Station.exe"

; Before anything is installed: take the Electron app off this PC. Its own
; uninstaller does it, as electron-builder's updates do (installUtil.nsh),
; keeping its app data - the Rust app reads the settings, history and window
; place from there when it first starts. Done before this installer makes
; its shortcuts, because the Electron uninstaller deletes the desktop and
; Start menu shortcuts by name, and the names are the same.
!macro NSIS_HOOK_PREINSTALL
  Push $R7
  Push $R8
  InitPluginsDir
  ReadRegStr $R7 HKCU "Software\${ELECTRON_GUID}" "InstallLocation"
  ${If} $R7 != ""
  ${AndIf} ${FileExists} "$R7\${ELECTRON_UNINSTALLER}"
    DetailPrint "Removing the Electron build of Falcon Scale Station from $R7"
    ; A copy runs it, as the original cannot delete itself while running;
    ; _?= makes it work in place and lets ExecWait wait for it.
    CopyFiles /SILENT "$R7\${ELECTRON_UNINSTALLER}" "$PLUGINSDIR\electron-uninstall.exe"
    ExecWait '"$PLUGINSDIR\electron-uninstall.exe" /S /KEEP_APP_DATA /currentuser _?=$R7' $R8
    DetailPrint "Electron uninstaller finished ($R8)"
    RMDir /r "$R7"
  ${EndIf}
  ; Its Start with Windows entry, which its uninstaller leaves behind. Electron
  ; names it after the app's user model id.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "com.falcongreen.scalestation"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "electron.app.Falcon Scale Station"
  Pop $R8
  Pop $R7
!macroend

; The Electron app's updater runs an installer with --updated /S --force-run
; and expects the app to start again afterwards; Tauri's installer restarts
; it only for /R. Both mean the same, so both are honoured.
!macro NSIS_HOOK_POSTINSTALL
  Push $R0
  ${If} ${Silent}
    ClearErrors
    ${GetOptions} $CMDLINE "--force-run" $R0
    ${IfNot} ${Errors}
      ClearErrors
      ${GetOptions} $CMDLINE "/R" $R0
      ${If} ${Errors}
        nsis_tauri_utils::RunAsUser "$INSTDIR\${MAINBINARYNAME}.exe" ""
      ${EndIf}
    ${EndIf}
  ${EndIf}
  Pop $R0
!macroend
