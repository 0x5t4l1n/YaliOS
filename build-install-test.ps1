$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $MyInvocation.MyCommand.Path
$stage = Join-Path $project 'target\installed-vhd'
$disk = Join-Path $stage 'yali-os.img'
$bootImage = Join-Path $project 'target\x86_64-yali\debug\bootimage-yali_os.bin'
$qemu = 'C:\Program Files\qemu\qemu-system-x86_64.exe'
$llvmObjcopy = 'C:\Program Files\LLVM\bin\llvm-objcopy.exe'
$imageSize = 64MB

New-Item -ItemType Directory -Force -Path $stage | Out-Null
Get-Process qemu-system-x86_64 -ErrorAction SilentlyContinue | Stop-Process -Force
$vsDevCmd = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat'
$buildCommand = "call `"$vsDevCmd`" -arch=x64 >nul && set `"PATH=C:\Users\$env:USERNAME\.cargo\bin;C:\Program Files\qemu;%PATH%`" && cargo bootimage"
Write-Host 'Building YaliOS boot image...'
cmd.exe /d /c $buildCommand
$bootExit = $LASTEXITCODE

if ($bootExit -ne 0 -or -not (Test-Path -LiteralPath $bootImage)) {
    Write-Host 'cargo bootimage did not produce the final image; using the freshly built bootloader ELF with LLVM objcopy.'
    $bootloader = Join-Path $project 'target\bootimage\bootloader\x86_64-bootloader\release\bootloader'
    if (-not (Test-Path -LiteralPath $bootloader)) { throw 'Fresh bootloader ELF was not found.' }
    if (-not (Test-Path -LiteralPath $llvmObjcopy)) { throw 'LLVM objcopy was not found.' }
    $stagedBoot = Join-Path $stage 'bootimage.bin'
    & $llvmObjcopy -I elf64-x86-64 -O binary $bootloader $stagedBoot
    if ($LASTEXITCODE -ne 0) { throw 'LLVM objcopy failed.' }
    $bootImage = $stagedBoot
}

Write-Host 'Creating a fresh test-only virtual hard disk...'
$diskStream = [IO.File]::Open($disk, [IO.FileMode]::Create, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
try { $diskStream.SetLength($imageSize) } finally { $diskStream.Dispose() }

Write-Host 'Installing the bootloader/kernel image at LBA 0...'
$source = [IO.File]::OpenRead($bootImage)
$target = [IO.File]::Open($disk, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::Read)
try { $source.CopyTo($target) } finally { $source.Dispose(); $target.Dispose() }

function Invoke-YaliBoot([int] $port, [string[]] $commands) {
    $process = Start-Process -FilePath $qemu -ArgumentList @('-boot','order=c','-drive',"format=raw,file=$disk",'-serial',"tcp:127.0.0.1:$port,server=on,wait=on",'-display','none') -WindowStyle Hidden -PassThru
    try {
        $client = $null
        for ($attempt = 0; $attempt -lt 40 -and $null -eq $client; $attempt++) {
            Start-Sleep -Milliseconds 100
            try { $client = New-Object Net.Sockets.TcpClient; $client.Connect('127.0.0.1', $port) } catch { $client = $null }
        }
        if ($null -eq $client) { throw 'Could not connect to QEMU serial console.' }
        $stream = $client.GetStream(); $stream.ReadTimeout = 500
        $buffer = New-Object byte[] 32768; $output = ''
        function Read-Serial { $text = ''; while ($true) { try { if (-not $stream.DataAvailable) { break }; $count = $stream.Read($buffer, 0, $buffer.Length); if ($count -gt 0) { $text += [Text.Encoding]::ASCII.GetString($buffer, 0, $count) } } catch { break } }; return $text }
        for ($i = 0; $i -lt 12; $i++) { Start-Sleep -Milliseconds 100; $output += Read-Serial }
        foreach ($command in $commands) {
            $bytes = [Text.Encoding]::ASCII.GetBytes($command)
            foreach ($byte in $bytes) { $stream.WriteByte($byte); if ($command.Length -gt 8) { Start-Sleep -Milliseconds 5 } }
            $enter = [Text.Encoding]::ASCII.GetBytes("`r`n"); $stream.Write($enter, 0, $enter.Length)
            Start-Sleep -Milliseconds 800; $output += Read-Serial
        }
        $client.Close(); return $output
    } finally {
        if ($process -and -not $process.HasExited) { Stop-Process -Id $process.Id -Force }
        Get-Process qemu-system-x86_64 -ErrorAction SilentlyContinue | Stop-Process -Force
    }
}

Write-Host 'First boot: explicitly formatting the newly created image and writing a file...'
$rootPassword = 'Root@123'
$userPassword = 'useruser'
Write-Host 'Test root username: root'
Write-Host 'Test root password: Root@123'
Write-Host 'Test user username: user'
Write-Host 'Test user password: useruser'
$first = Invoke-YaliBoot 5592 @('format /dev/yali-test','setup-users',$rootPassword,$userPassword,'whoami','mkdir /install-test','write /install-test/installed.txt persistent-install')
Write-Host $first

Write-Host 'Second boot: mounting the same image and reading the file back...'
$second = Invoke-YaliBoot 5593 @('user',$userPassword,'whoami','id','write /install-test/installed.txt denied','write /home/user/note user-data','cat /home/user/note','cat /install-test/installed.txt','ls /')
Write-Host $second
if ($first -notmatch 'accounts created; logged in as root' -or $first -notmatch "root") { throw 'Root setup/login verification failed.' }
if ($second -notmatch 'login successful' -or $second -notmatch "user" -or $second -notmatch 'uid=1000' -or $second -notmatch 'permission denied' -or $second -notmatch 'user-data' -or $second -notmatch 'persistent-install' -or $second -notmatch 'd install-test') { throw 'Account, permission, or persistence verification failed.' }
Write-Host 'PASS: YaliOS booted from the installed single-disk image and persistence survived reboot.'
