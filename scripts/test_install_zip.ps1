# Requires PowerShell 5+ or pwsh. Proves a tampered payload is rejected before
# the installer replaces an existing prefix, even when the outer SHA-256 sidecar
# is recomputed. -AllowUnverified is not used: BLAKE3SUMS is still checked.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

$root = Join-Path ([IO.Path]::GetTempPath()) ("arandu-zip-test-" + [guid]::NewGuid().ToString('n'))
$version = '0.1.11-rc.1'
$target = 'x86_64-pc-windows-msvc'
$treeName = "arandu-$version"
$tree = Join-Path $root $treeName
$prefix = Join-Path $root 'prefix'
$archive = Join-Path $root "$treeName-$target.zip"
$installer = Join-Path $PSScriptRoot 'install-from-zip.ps1'

function Add-ZipFile([System.IO.Compression.ZipArchive]$zip, [string]$name, [byte[]]$bytes) {
    $entry = $zip.CreateEntry($name, [System.IO.Compression.CompressionLevel]::NoCompression)
    $entry.ExternalAttributes = 0
    $stream = $entry.Open()
    try { $stream.Write($bytes, 0, $bytes.Length) } finally { $stream.Dispose() }
}

New-Item -ItemType Directory -Force (Join-Path $tree 'bin'), (Join-Path $tree 'share/arandu/stdlib'), (Join-Path $tree "lib/$target") | Out-Null
$stub = @"
#!/usr/bin/env python3
import hashlib, sys
from pathlib import Path
if sys.argv[1:] == ['--version']:
    print('arandu $version')
elif len(sys.argv) >= 2 and sys.argv[1] == 'hash-file':
    print(hashlib.sha256(Path(sys.argv[2]).read_bytes()).hexdigest())
elif sys.argv[1:] == ['doctor']:
    raise SystemExit(0)
else:
    raise SystemExit(2)
"@
$stubBytes = [Text.Encoding]::UTF8.GetBytes($stub.Replace("`r`n", "`n"))
[IO.File]::WriteAllBytes((Join-Path $tree 'bin/arandu.exe'), $stubBytes)
[IO.File]::WriteAllBytes((Join-Path $tree 'bin/arandu_cli.exe'), $stubBytes)
[IO.File]::WriteAllBytes((Join-Path $tree 'bin/arandu-lsp.exe'), $stubBytes)
$unixHost = $false
if ((Get-Variable -Name IsLinux -Scope Global -ErrorAction SilentlyContinue) -and ($IsLinux -or $IsMacOS)) {
    $unixHost = $true
}
if ($unixHost) {
    foreach ($name in 'arandu.exe','arandu_cli.exe','arandu-lsp.exe') {
        & chmod 755 (Join-Path $tree "bin/$name")
    }
}
$payloadPath = Join-Path $tree 'share/arandu/stdlib/test.aru'
$original = [Text.Encoding]::ASCII.GetBytes("fn main() {}`n")
[IO.File]::WriteAllBytes($payloadPath, $original)
[IO.File]::WriteAllBytes((Join-Path $tree "lib/$target/arandu_runtime.lib"), [byte[]](1, 2, 3))
[IO.File]::WriteAllText((Join-Path $tree 'LICENSE-MIT'), "mit`n")
[IO.File]::WriteAllText((Join-Path $tree 'LICENSE-APACHE'), "apache`n")
$manifest = "{`"schema`":1,`"version`":`"$version`",`"target`":`"$target`",`"archive`":`"zip`"}`n"
[IO.File]::WriteAllText((Join-Path $tree 'release-manifest.json'), $manifest)

function Sha256([string]$path) {
    $sha = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.File]::OpenRead($path)
    try { return ([BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '').ToLowerInvariant() }
    finally { $stream.Dispose(); $sha.Dispose() }
}

$files = @(
    'bin/arandu.exe',
    'bin/arandu_cli.exe',
    'bin/arandu-lsp.exe',
    'share/arandu/stdlib/test.aru',
    "lib/$target/arandu_runtime.lib",
    'LICENSE-MIT',
    'LICENSE-APACHE',
    'release-manifest.json'
)
$lines = foreach ($relative in $files) {
    '{0}  {1}' -f (Sha256 (Join-Path $tree $relative)), $relative
}
# Recalculate nothing for the payload: replace it after the sum was captured.
$payloadSum = ($lines | Where-Object { $_ -like '*test.aru' })
[IO.File]::WriteAllBytes($payloadPath, [Text.Encoding]::ASCII.GetBytes("tampered`n"))
[IO.File]::WriteAllText((Join-Path $tree 'BLAKE3SUMS'), (($lines -join "`n") + "`n"))

if (Test-Path $archive) { Remove-Item $archive }
$zip = [IO.Compression.ZipFile]::Open($archive, [IO.Compression.ZipArchiveMode]::Create)
try {
    Get-ChildItem -File -Recurse -LiteralPath $tree | ForEach-Object {
        $relative = $_.FullName.Substring($tree.Length + 1).Replace('\', '/')
        $bytes = [IO.File]::ReadAllBytes($_.FullName)
        Add-ZipFile $zip "$treeName/$relative" $bytes
    }
} finally { $zip.Dispose() }

$archiveHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
Set-Content -LiteralPath "$archive.sha256" -Value $archiveHash -Encoding ascii

$versionDir = Join-Path $prefix $treeName
New-Item -ItemType Directory -Force $versionDir | Out-Null
Set-Content -LiteralPath (Join-Path $versionDir 'MARKER') -Value 'keep' -Encoding ascii

$failed = $false
try {
    & $installer -Archive $archive -Prefix $prefix -NoModifyPath
} catch {
    $failed = $true
    if ($_.Exception.Message -notmatch 'BLAKE3SUMS mismatch') {
        throw
    }
}
if (-not $failed) { throw 'installer accepted a payload whose BLAKE3SUMS no longer matches' }
$marker = Get-Content -Raw -LiteralPath (Join-Path $versionDir 'MARKER')
if ($marker.Trim() -ne 'keep') { throw 'previous prefix was replaced' }
Write-Host "zip installer rejected the tampered payload and kept the previous prefix"
Remove-Item -Recurse -Force $root
