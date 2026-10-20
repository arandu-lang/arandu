param(
    [Parameter(Mandatory=$true, Position=0)][string]$Archive,
    [string]$Prefix = "$env:LOCALAPPDATA\Arandu",
    [switch]$AllowUnverified,
    [switch]$NoModifyPath
)
$ErrorActionPreference = 'Stop'
$archivePath = (Resolve-Path -LiteralPath $Archive).Path
$fileName = [IO.Path]::GetFileName($archivePath)
if ($fileName -notmatch '^arandu-(?<version>[0-9][0-9A-Za-z.-]*)-(?<target>x86_64-pc-windows-msvc)\.zip$') {
    throw "unsupported Arandu archive name: $fileName"
}
$version = $Matches.version
$target = $Matches.target
$sidecar = "$archivePath.sha256"
if (Test-Path -LiteralPath $sidecar) {
    $expected = ((Get-Content -Raw -LiteralPath $sidecar).Trim() -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
    if ($expected -ne $actual) { throw "SHA256 mismatch for $fileName; archive is corrupt or tampered" }
} elseif (-not $AllowUnverified) {
    throw "missing $fileName.sha256; use -AllowUnverified only for local development"
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$stage = Join-Path ([IO.Path]::GetTempPath()) ("arandu-install-" + [guid]::NewGuid())
$treeName = "arandu-$version"
$tree = Join-Path $stage $treeName
$seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
try {
    New-Item -ItemType Directory -Force $stage | Out-Null
    $zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName
            $components = $name.Split('/')
            if ($name.Contains('\') -or $name.StartsWith('/') -or $components -contains '' -or $components -contains '.' -or $components -contains '..') { throw "unsafe zip entry: $name" }
            $unixType = (($entry.ExternalAttributes -shr 16) -band 0xF000)
            if ($unixType -ne 0 -and $unixType -ne 0x8000) { throw "unsupported zip entry type: $name" }
            if (-not $name.StartsWith("$treeName/", [StringComparison]::Ordinal)) { throw "entry outside package root: $name" }
            if (-not $seen.Add($name)) { throw "duplicate zip entry: $name" }
            $relative = $name.Substring($treeName.Length + 1)
            $fixed = @('bin/arandu.exe','bin/arandu_cli.exe','bin/arandu-lsp.exe',"lib/$target/arandu_runtime.lib",'BLAKE3SUMS','LICENSE-MIT','LICENSE-APACHE','release-manifest.json')
            if ($relative -notin $fixed -and -not $relative.StartsWith('share/arandu/stdlib/', [StringComparison]::Ordinal)) {
                throw "unexpected zip content: $name"
            }
            $destination = Join-Path $tree ($relative.Replace('/', [IO.Path]::DirectorySeparatorChar))
            $parent = Split-Path -Parent $destination
            New-Item -ItemType Directory -Force $parent | Out-Null
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $destination, $false)
        }
    } finally { $zip.Dispose() }
    foreach ($required in 'bin/arandu.exe','bin/arandu-lsp.exe',"lib/$target/arandu_runtime.lib",'share/arandu/stdlib','BLAKE3SUMS','release-manifest.json','LICENSE-MIT','LICENSE-APACHE') {
        if (-not (Test-Path -LiteralPath (Join-Path $tree $required))) { throw "archive missing $required" }
    }
    $manifest = Get-Content -Raw -LiteralPath "$tree/release-manifest.json" | ConvertFrom-Json
    if ($manifest.schema -ne 1 -or $manifest.version -ne $version -or $manifest.target -ne $target -or $manifest.archive -ne 'zip') {
        throw 'release manifest does not match archive identity'
    }
    $sumFile = Join-Path $tree 'BLAKE3SUMS'
    $treeFull = [IO.Path]::GetFullPath($tree).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    $verified = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $sumCount = 0
    foreach ($line in Get-Content -LiteralPath $sumFile) {
        if ([string]::IsNullOrWhiteSpace($line) -or $line.StartsWith('#')) { continue }
        if ($line -notmatch '^(?<hash>[0-9a-f]{64})  (?<path>.+)$') { throw 'malformed BLAKE3SUMS entry' }
        $expected = $Matches.hash
        $relative = $Matches.path
        if ($relative.Contains('\') -or $relative.StartsWith('/') -or $relative.Split('/') -contains '..' -or $relative.Split('/') -contains '.') {
            throw "unsafe BLAKE3SUMS path: $relative"
        }
        if (-not $verified.Add($relative)) { throw "duplicate BLAKE3SUMS entry: $relative" }
        $file = [IO.Path]::GetFullPath((Join-Path $tree ($relative.Replace('/', [IO.Path]::DirectorySeparatorChar))))
        if (-not $file.StartsWith($treeFull, [StringComparison]::OrdinalIgnoreCase) -or -not (Test-Path -LiteralPath $file -PathType Leaf)) {
            throw "BLAKE3SUMS file is missing or outside package: $relative"
        }
        $actual = (& "$tree/bin/arandu.exe" hash-file $file).Trim()
        if ($LASTEXITCODE -ne 0 -or $actual -ne $expected) { throw "BLAKE3SUMS mismatch for $relative" }
        $sumCount++
    }
    if ($sumCount -eq 0) { throw 'BLAKE3SUMS is empty' }
    $packageFiles = @(Get-ChildItem -File -Recurse -LiteralPath $tree | Where-Object { $_.FullName -ne $sumFile })
    if ($packageFiles.Count -ne $sumCount) { throw 'BLAKE3SUMS does not cover every package file' }
    foreach ($file in $packageFiles) {
        $relative = $file.FullName.Substring($treeFull.Length).Replace('\', '/')
        if (-not $verified.Contains($relative)) { throw "BLAKE3SUMS is missing $relative" }
    }
    $reportedVersion = (& "$tree/bin/arandu.exe" --version).Trim()
    if ($LASTEXITCODE -ne 0 -or $reportedVersion -ne "arandu $version") {
        throw "binary version '$reportedVersion' does not match package version $version"
    }
    New-Item -ItemType Directory -Force $Prefix, "$Prefix\bin" | Out-Null
    $versionDir = Join-Path $Prefix $treeName
    $backup = "$versionDir.old.$PID"
    if (Test-Path -LiteralPath $versionDir) { Move-Item -LiteralPath $versionDir -Destination $backup }
    try {
        if ($env:ARANDU_TEST_FAIL_PUBLISH -eq '1') { throw 'injected publish failure' }
        Move-Item -LiteralPath $tree -Destination $versionDir
    } catch {
        if (Test-Path -LiteralPath $backup) { Move-Item -LiteralPath $backup -Destination $versionDir }
        throw
    }
    if (Test-Path -LiteralPath $backup) { Remove-Item -Recurse -Force -LiteralPath $backup }
    foreach ($command in 'arandu','arandu-lsp') {
        $launcher = "$Prefix\bin\$command.cmd"
        $temporary = "$launcher.new.$PID"
        "@echo off`r`n`"%~dp0..\$treeName\bin\$command.exe`" %*`r`n" | Set-Content -Encoding ascii -LiteralPath $temporary
        Move-Item -Force -LiteralPath $temporary -Destination $launcher
    }
    $binPath = [IO.Path]::GetFullPath("$Prefix\bin").TrimEnd('\')
    if (-not $NoModifyPath) {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = @($userPath -split ';' | Where-Object { $_ })
        if (-not ($entries | Where-Object { $_.Trim().TrimEnd('\') -eq $binPath })) {
            $newPath = (@($entries) + $binPath) -join ';'
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            Write-Host "added $binPath to the user PATH (open a new terminal)"
        } else {
            Write-Host "$binPath is already in the user PATH"
        }
    } else {
        Write-Host "PATH unchanged; add $binPath manually"
    }
    & "$Prefix\bin\arandu.cmd" doctor
    Write-Host "installed $treeName under $Prefix"
} finally {
    if (Test-Path -LiteralPath $stage) { Remove-Item -Recurse -Force -LiteralPath $stage }
}
