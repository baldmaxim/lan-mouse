# Удаляет ярлыки Lan Mouse с рабочего стола и из автозагрузки.
$ErrorActionPreference = 'Stop'

foreach ($folder in @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('Startup'))) {
    $path = Join-Path $folder 'Lan Mouse.lnk'
    if (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path -Force
        Write-Host "Удалён ярлык: $path"
    }
}
Write-Host 'Готово.'
