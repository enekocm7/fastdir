$ErrorActionPreference = 'Stop'

[array]$keys = Get-UninstallRegistryKey -SoftwareName 'FastDir'

if ($keys.Count -eq 1) {
  $packageArgs = @{
    packageName    = $env:ChocolateyPackageName
    fileType       = 'exe'
    # Inno Setup uninstaller; /KEEPDATA=1 keeps %APPDATA%\FastDir (settings).
    silentArgs     = '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /KEEPDATA=1'
    validExitCodes = @(0)
    file           = ($keys[0].UninstallString -replace '"', '')
  }
  Uninstall-ChocolateyPackage @packageArgs
} elseif ($keys.Count -eq 0) {
  Write-Warning "$env:ChocolateyPackageName has already been uninstalled by other means."
} else {
  Write-Warning "$($keys.Count) matches found; not uninstalling to avoid removing the wrong program."
  $keys | ForEach-Object { Write-Warning "- $($_.DisplayName)" }
}
