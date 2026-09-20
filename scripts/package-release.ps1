param(
    [string]$Version = $env:VERSION,
    [string]$Target = $env:TARGET,
    [string]$OutDir = $env:OUT_DIR,
    [string]$SourceDateEpoch = $env:SOURCE_DATE_EPOCH
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Version) { $Version = (Select-String '^version = "([^"]+)"' "$root/crates/arandu_cli/Cargo.toml").Matches[0].Groups[1].Value }
if (-not $Target) { $Target = ((rustc -vV | Select-String '^host: ').Line -replace '^host: ', '') }
if (-not $OutDir) { $OutDir = Join-Path $root 'dist' }
if (-not $SourceDateEpoch) { $SourceDateEpoch = (git -C $root log -1 --format=%ct) }
if ($Target -ne 'x86_64-pc-windows-msvc') { throw "Windows package requires x86_64-pc-windows-msvc, got $Target" }

cargo build --locked -p arandu_cli -p arandu_lsp -p arandu_runtime --release --manifest-path "$root/Cargo.toml"
$stage = Join-Path ([IO.Path]::GetTempPath()) ("arandu-package-" + [guid]::NewGuid())
$tree = Join-Path $stage "arandu-$Version"
try {
    New-Item -ItemType Directory -Force "$tree/bin", "$tree/lib/$Target", "$tree/share/arandu" | Out-Null
    Copy-Item "$root/target/release/arandu_cli.exe" "$tree/bin/arandu.exe"
    Copy-Item "$root/target/release/arandu_cli.exe" "$tree/bin/arandu_cli.exe"
    Copy-Item "$root/target/release/arandu-lsp.exe" "$tree/bin/arandu-lsp.exe"
    Copy-Item "$root/target/release/arandu_runtime.lib" "$tree/lib/$Target/arandu_runtime.lib"
    Copy-Item -Recurse "$root/stdlib" "$tree/share/arandu/stdlib"
    Copy-Item "$root/LICENSE-MIT", "$root/LICENSE-APACHE" $tree
    [ordered]@{schema=1; version=$Version; target=$Target; components=@('arandu','arandu-lsp','runtime','stdlib'); archive='zip'} |
        ConvertTo-Json | Set-Content -Encoding utf8NoBOM "$tree/release-manifest.json"
    $hashLines = Get-ChildItem -File -Recurse $tree | Sort-Object { $_.FullName.Substring($tree.Length).Replace('\','/') } | ForEach-Object {
        $relative = $_.FullName.Substring($tree.Length + 1).Replace('\','/')
        $hash = & "$tree/bin/arandu.exe" hash-file $_.FullName
        "$hash  $relative"
    }
    $hashLines | Set-Content -Encoding ascii "$tree/BLAKE3SUMS"
    New-Item -ItemType Directory -Force $OutDir | Out-Null
    $zip = Join-Path $OutDir "arandu-$Version-$Target.zip"
    cargo run --locked -p xtask -- package-archive --source $tree --output $zip --epoch $SourceDateEpoch
    cargo run --locked -p xtask -- validate-archive $zip --root "arandu-$Version" --target $Target --version $Version
    $hash = & "$tree/bin/arandu.exe" hash-file $zip
    $hash | Set-Content -Encoding ascii "$zip.blake3"
    "$hash  $([IO.Path]::GetFileName($zip))" | Set-Content -Encoding ascii "$zip.blake3sum"
    $sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLowerInvariant()
    $sha256 | Set-Content -Encoding ascii "$zip.sha256"
    "$sha256  $([IO.Path]::GetFileName($zip))" | Set-Content -Encoding ascii "$zip.sha256sum"
    Write-Host "wrote $zip"
} finally {
    if (Test-Path -LiteralPath $stage) { Remove-Item -Recurse -Force -LiteralPath $stage }
}
