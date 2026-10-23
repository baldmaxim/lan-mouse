# Создаёт ярлыки Lan Mouse: на рабочем столе и в автозагрузке (запуск свёрнутым в трей).
$ErrorActionPreference = 'Stop'

$dir = $PSScriptRoot
$exe = Join-Path $dir 'lan-mouse.exe'
if (-not (Test-Path -LiteralPath $exe)) {
    Write-Host "Не найден lan-mouse.exe рядом со скриптом: $dir"
    exit 1
}

function New-LanMouseShortcut([string]$folder, [string]$arguments) {
    $path = Join-Path $folder 'Lan Mouse.lnk'
    $shell = New-Object -ComObject WScript.Shell
    $lnk = $shell.CreateShortcut($path)
    $lnk.TargetPath = $exe
    $lnk.Arguments = $arguments
    $lnk.WorkingDirectory = $dir
    $lnk.IconLocation = "$exe,0"
    $lnk.Description = 'Lan Mouse'
    $lnk.Save()
    Write-Host "Создан ярлык: $path"
}

New-LanMouseShortcut ([Environment]::GetFolderPath('Desktop')) ''
New-LanMouseShortcut ([Environment]::GetFolderPath('Startup')) '--hidden'
Write-Host 'Готово. Lan Mouse будет запускаться при входе в Windows (в трее).'
