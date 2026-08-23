$ErrorActionPreference = 'Stop'

$packageName = 'pgpilot'
$repo        = 'kavin-vs/pgpilot'
$version     = $env:ChocolateyPackageVersion
$tag         = "v$version"
$target      = 'x86_64-pc-windows-msvc'
$zipName     = "pgpilot-$tag-$target.zip"
$baseUrl     = "https://github.com/$repo/releases/download/$tag"

# checksums.txt (sha256sum output) is published alongside every release's
# binaries -- read per-version so this script never needs hand-editing when
# a new pgpilot version is packed and pushed.
$checksums = (Invoke-WebRequest -UseBasicParsing "$baseUrl/checksums.txt").Content
$line = ($checksums -split "`r?`n") | Where-Object { $_ -match [regex]::Escape($zipName) }
if (-not $line) {
  throw "checksums.txt for $tag has no entry for $zipName"
}
$checksum64 = ($line.Trim() -split '\s+')[0]

Install-ChocolateyZipPackage -PackageName $packageName `
  -Url64bit "$baseUrl/$zipName" `
  -UnzipLocation (Split-Path -Parent $MyInvocation.MyCommand.Definition) `
  -Checksum64 $checksum64 `
  -ChecksumType64 'sha256'
