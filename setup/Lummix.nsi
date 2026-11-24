; One-file, per-user installer. Windows Installer owns the app and both shortcuts.
Unicode true
RequestExecutionLevel user
ManifestDPIAware true
!include "LogicLib.nsh"
!include "x64.nsh"

Name "Lummix"
Caption "Installing Lummix"
OutFile "${SETUP_EXE}"
Icon "..\windows\packaging\Lummix.ico"
VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=1033 "ProductName" "Lummix"
VIAddVersionKey /LANG=1033 "FileDescription" "Lummix Setup"
VIAddVersionKey /LANG=1033 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=1033 "CompanyName" "Lummix"
VIAddVersionKey /LANG=1033 "LegalCopyright" "Lummix contributors"
BrandingText "Lummix"
Page instfiles
ShowInstDetails nevershow
AutoCloseWindow true
SetCompress off

Section
  SetErrorLevel 0
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP|MB_OK "Lummix needs 64-bit Windows." /SD IDOK
    SetErrorLevel 1633
    Quit
  ${EndIf}
  SetShellVarContext current
  SetDetailsPrint textonly
  DetailPrint "Preparing Lummix…"
  SetOverwrite on
  ClearErrors
  SetOutPath "$LOCALAPPDATA\Lummix\Installer\${PRODUCT_CODE}"
  File /oname=Lummix.msi "${PAYLOAD_MSI}"
  ${If} ${Errors}
    MessageBox MB_ICONSTOP|MB_OK "Windows could not save the installer. Check your free disk space, then open Lummix Setup again." /SD IDOK
    SetErrorLevel 1603
    Quit
  ${EndIf}
  DetailPrint "Installing Lummix for this Windows account…"
  ; Restore missing shortcuts when the same installer runs again.
  System::Call 'msi::MsiQueryProductStateW(w "${PRODUCT_CODE}") i.r1'
  StrCpy $2 ""
  ${If} $1 = 5
    StrCpy $2 "REINSTALL=ALL REINSTALLMODE=omus"
  ${EndIf}
  ClearErrors
  ExecWait '"$SYSDIR\msiexec.exe" /i "$OUTDIR\Lummix.msi" $2 /qn /norestart' $0
  ${If} ${Errors}
    StrCpy $0 1601
  ${EndIf}
  SetErrorLevel $0
  ${If} $0 = 3010
    MessageBox MB_ICONINFORMATION|MB_OK "Lummix is installed. Restart Windows, then open Lummix from Start." /SD IDOK
    Quit
  ${ElseIf} $0 = 1618
    MessageBox MB_ICONINFORMATION|MB_OK "Another Windows installation is running. Open Lummix Setup again when it finishes." /SD IDOK
    Quit
  ${ElseIf} $0 != 0
    MessageBox MB_ICONSTOP|MB_OK "Windows Installer could not finish (error $0). Open Lummix Setup again to retry." /SD IDOK
    Quit
  ${EndIf}
  IfSilent done
  IfFileExists "$SMPROGRAMS\Lummix\Lummix.lnk" 0 openFromStart
  ClearErrors
  ExecShell "open" "$SMPROGRAMS\Lummix\Lummix.lnk"
  IfErrors openFromStart done
  openFromStart:
    MessageBox MB_ICONINFORMATION|MB_OK "Lummix is installed. Open Lummix from the Windows Start menu."
  done:
SectionEnd
