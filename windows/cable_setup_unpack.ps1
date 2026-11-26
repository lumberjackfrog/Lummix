# The native host holds a read-only lock on the SHA-256-pinned archive throughout extraction.
param([Parameter(Mandatory=$true)][string]$Folder)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = Join-Path (Split-Path -Parent $Folder) 'vendor.zip'
[IO.Compression.ZipFile]::ExtractToDirectory($archive, $Folder)
'{"extracted":true}'
