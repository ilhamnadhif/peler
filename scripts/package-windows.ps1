# Builds TernaryShell for Windows (x64) into dist\:
#   TernaryShell-<version>-windows-x64-setup.exe   installer (Inno Setup 6; per user, no administrator rights)
#   TernaryShell-<version>-windows-x64.zip         portable: ternaryshell.exe alone
#
#   pwsh scripts/package-windows.ps1            # builds the release binary first
#   pwsh scripts/package-windows.ps1 -NoBuild   # packages target\release\ternaryshell.exe
#
# Needs Rust (MSVC toolchain) and Inno Setup 6 (ISCC.exe; preinstalled on GitHub's Windows runners).
param([switch]$NoBuild)
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$dist = Join-Path (Get-Location) 'dist'
New-Item -ItemType Directory -Force -Path $dist | Out-Null

if (-not $NoBuild) {
    Write-Host "==> cargo build --release ($version)"
    cargo build --release --locked
    if ($LASTEXITCODE) { throw 'cargo build failed' }
}
$exe = (Resolve-Path 'target\release\ternaryshell.exe').Path

# It starts and is the version being packaged (a GUI program: its output only reaches a
# redirected stream).
$out = Join-Path $env:TEMP "ternaryshell-version-$PID.txt"
$p = Start-Process -FilePath $exe -ArgumentList '--version' -RedirectStandardOutput $out -NoNewWindow -Wait -PassThru
$said = (Get-Content $out -Raw).Trim()
Remove-Item $out
if ($p.ExitCode -ne 0 -or $said -ne "TernaryShell $version") { throw "ternaryshell.exe --version said '$said' (exit $($p.ExitCode)), expected TernaryShell $version" }
Write-Host ("==> $said, ternaryshell.exe {0:N1} MB" -f ((Get-Item $exe).Length / 1MB))

$zip = Join-Path $dist "TernaryShell-$version-windows-x64.zip"
$stage = Join-Path $env:TEMP "ternaryshell-zip-$PID"
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Copy-Item $exe, README.md, LICENSE $stage
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -CompressionLevel Optimal -Force
Remove-Item -Recurse $stage
Write-Host "==> $zip"

$iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 (ISCC.exe) is not installed' }
& $iscc /Qp "/DAppVersion=$version" "/DSourceExe=$exe" "/DOutputDir=$dist" 'packaging\windows\ternaryshell.iss'
if ($LASTEXITCODE) { throw 'ISCC failed' }

Get-ChildItem $dist -Filter "TernaryShell-$version-windows-*" | ForEach-Object { Write-Host ("    {0}  {1:N1} MB" -f $_.Name, ($_.Length / 1MB)) }
