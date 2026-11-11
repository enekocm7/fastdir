$ErrorActionPreference = 'Stop'

# Filled in by the release workflow for every version.
$version = '0.3.2'
$checksum = 'e508ecc61b4aa2e028f4463f2e00e0d03093c09d0a171483b8040c7d31d353ac'

$packageArgs = @{
  packageName    = $env:ChocolateyPackageName
  fileType       = 'exe'
  url64bit       = "https://github.com/enekocm7/fastdir/releases/download/v$version/FastDir-$version-setup-x64.exe"
  checksum64     = $checksum
  checksumType64 = 'sha256'
  # Inno Setup: all-users install, no UI, no reboot.
  silentArgs     = '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /ALLUSERS'
  validExitCodes = @(0)
  softwareName   = 'FastDir'
}

Install-ChocolateyPackage @packageArgs
