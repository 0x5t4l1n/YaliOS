@echo off
cd /d "%~dp0"
if not exist "%CD%\target\installed-vhd\yali-os.img" (
    echo Installed disk not found. Run build-install-test.cmd first.
    exit /b 1
)
"C:\Program Files\qemu\qemu-system-x86_64.exe" -boot order=c -drive format=raw,file="%CD%\target\installed-vhd\yali-os.img" -serial stdio -display none
pause
