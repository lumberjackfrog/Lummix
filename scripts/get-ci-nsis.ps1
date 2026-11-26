# Restore the reviewed installer compiler inside the Windows Actions runner.
$ErrorActionPreference = 'Stop'
if (!$env:RUNNER_TEMP -or !$env:GITHUB_ENV) { throw 'Expected a GitHub Actions runner.' }
$file = Join-Path $env:RUNNER_TEMP 'nsis-tool.exe'
$toolDirectory = Join-Path $env:RUNNER_TEMP 'nsis-tools'
curl.exe -fLsS --retry 2 -o $file 'https://downloads.sourceforge.net/project/nsis/NSIS%203/3.12/nsis-3.12-setup.exe'
if ($LASTEXITCODE) { throw 'NSIS download failed.' }
if ((Get-FileHash -LiteralPath $file).Hash -ne '3BC2B06253A7E4957111BE152AC6A536E0C7478A706E19DA814038DB5D706495') { throw 'NSIS hash mismatch.' }
7z x -y "-o$toolDirectory" $file | Out-Null
if ($LASTEXITCODE) { throw 'NSIS extraction failed.' }
"NSIS_COMPILER=$toolDirectory\makensis.exe" >> $env:GITHUB_ENV
