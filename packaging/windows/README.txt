Haze for Windows
================

Tone editor, librarian and practice rig for the BOSS KATANA MkII.

1. Install the BOSS KATANA MkII USB driver (the same one BOSS Tone Studio uses):
   https://www.boss.info/global/support/by_product/katana_mkii/updates_drivers/
2. Run haze.exe. Nothing else to install; it is portable.
   Windows may warn "Windows protected your PC" (the exe is not code-signed):
   More info -> Run anyway.
3. Close BOSS Tone Studio first: only one program can hold the amp's MIDI port.

Your library lives in %APPDATA%\katana-studio. Exports go to %USERPROFILE%\Katana.
Your existing Tone Studio library is found automatically (Library -> Import).

Command line (from cmd or PowerShell, in this folder):
  haze import C:\path\to\tones.tsl
  haze import-windows
  haze list

Source, issues, Linux build: https://github.com/wiktor-jozwiak/haze
