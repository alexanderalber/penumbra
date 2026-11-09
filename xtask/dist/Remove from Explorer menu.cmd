@echo off
rem Removes the "Open with penumbra" entry from the right-click menu of folders.
for %%k in (shell Background\shell) do (
    reg delete "HKCU\Software\Classes\Directory\%%k\penumbra" /f >nul 2>nul
)
echo The entry is gone.
pause
