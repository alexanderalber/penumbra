@echo off
rem Adds "Open with penumbra" to the right-click menu of folders, for this Windows user only.
rem Run it again after moving this folder.
setlocal
set "exe=%~dp0penumbra.exe"
rem Delayed expansion from here on: the path may hold spaces, brackets or an ampersand.
setlocal EnableDelayedExpansion
if not exist "!exe!" (
    echo penumbra.exe is not next to this script. Unpack the whole zip first.
    pause
    exit /b 1
)
set "open=\"!exe!\" --project \"%%V\""
for %%k in (shell Background\shell) do (
    reg add "HKCU\Software\Classes\Directory\%%k\penumbra" /ve /d "Open with penumbra" /f >nul || goto failed
    reg add "HKCU\Software\Classes\Directory\%%k\penumbra" /v Icon /d "!exe!,0" /f >nul || goto failed
    reg add "HKCU\Software\Classes\Directory\%%k\penumbra\command" /ve /d "!open!" /f >nul || goto failed
)
echo Done. Right-click a folder: "Open with penumbra".
echo On Windows 11 the entry is under "Show more options".
pause
exit /b 0

:failed
echo The entry could not be added.
pause
exit /b 1
