param([switch]$Debug)
$ErrorActionPreference = 'Stop'
$component = Split-Path $PSScriptRoot -Parent
$rtk = Join-Path $component 'third_party/rtk'
function Get-CargoTargetDirectory([string]$Manifest) {
    # Cargo resolves build.target-dir too; PS 5.1 needs UTF-8 decoding (PS 7 defaults to it).
    $previousEncoding = [Console]::OutputEncoding
    try {
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        $metadata = & cargo +1.91.0 metadata --format-version 1 --no-deps --locked --manifest-path $Manifest
        if ($LASTEXITCODE -ne 0) { throw "Cargo metadata failed for $Manifest" }
        $directory = ($metadata | ConvertFrom-Json).target_directory
        if (!$directory -or ![IO.Path]::IsPathRooted($directory)) {
            throw "Cargo metadata has no absolute target directory for $Manifest"
        }
        return $directory
    } finally {
        [Console]::OutputEncoding = $previousEncoding
    }
}
# Match setup-rtk.sh's applied-tree digest; metadata and Cargo output are excluded.
function Get-RtkTreeDigest([string]$Root) {
    $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar)
    $paths = @{}
    foreach ($item in Get-ChildItem -LiteralPath $rootPath -Recurse -Force) {
        $relative = $item.FullName.Substring($rootPath.Length + 1).Replace('\', '/')
        if ($relative -match '^(\.git|target)(/|$)|^\.anolisa-rtk-[^/]+$') { continue }
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "RTK source contains an unsupported link: $relative"
        }
        if (!$item.PSIsContainer) { $paths["./$relative"] = $item.FullName }
    }
    [string[]]$names = @($paths.Keys)
    [Array]::Sort($names, [StringComparer]::Ordinal)
    $stream = [IO.MemoryStream]::new()
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        foreach ($name in $names) {
            $hash = (Get-FileHash -LiteralPath $paths[$name] -Algorithm SHA256).Hash.ToLowerInvariant()
            $bytes = [Text.Encoding]::UTF8.GetBytes($name + [char]0 + $hash + '  ' + $name + "`n")
            $stream.Write($bytes, 0, $bytes.Length)
        }
        return [BitConverter]::ToString($sha.ComputeHash($stream.ToArray())).Replace('-', '').ToLowerInvariant()
    } finally {
        $sha.Dispose()
        $stream.Dispose()
    }
}
# The existing setup script remains the single source of the pinned revision.
$source = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'setup-rtk.sh') -Raw
$revision = [regex]::Match($source, '(?m)^RTK_COMMIT="([0-9a-f]{40})"').Groups[1].Value
$repository = [regex]::Match($source, '(?m)^RTK_REPOSITORY="([^"]+)"').Groups[1].Value
if (!$revision -or !$repository) { throw 'RTK source pin is missing from setup-rtk.sh' }
$patches = [regex]::Match($source, '(?m)^PATCHES=\(([^)]+)\)').Groups[1].Value.Split(' ', [StringSplitOptions]::RemoveEmptyEntries)
if (!$patches.Count) { throw 'RTK patch order is missing from setup-rtk.sh' }
$patchBytes = [IO.MemoryStream]::new()
foreach ($patch in $patches) {
    $contents = [IO.File]::ReadAllText((Join-Path $component "third_party/patches/$patch")).Replace("`r`n", "`n")
    $bytes = [Text.Encoding]::UTF8.GetBytes($patch + [char]0 + $contents)
    $patchBytes.Write($bytes, 0, $bytes.Length)
}
$patchDigest = [BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($patchBytes.ToArray())).Replace("-", "").ToLowerInvariant()
$patchBytes.Dispose()
if (Test-Path -LiteralPath $rtk) {
    $marker = Get-Content -LiteralPath (Join-Path $rtk '.anolisa-rtk-commit') -Raw
    if ($marker.Trim() -ne $revision) { throw 'RTK checkout does not match the pinned revision' }
    $patchMarker = Join-Path $rtk '.anolisa-rtk-patches'
    if (!(Test-Path -LiteralPath $patchMarker) -or (Get-Content -LiteralPath $patchMarker -Raw).Trim() -ne $patchDigest) {
        throw 'RTK patches have changed; remove the generated RTK checkout and retry'
    }
    $treeMarker = Join-Path $rtk '.anolisa-rtk-tree'
    if (!(Test-Path -LiteralPath $treeMarker) -or (Get-Content -LiteralPath $treeMarker -Raw).Trim() -ne (Get-RtkTreeDigest $rtk)) {
        throw 'RTK applied source has changed or its digest is missing; preserve edits and prepare a clean checkout'
    }
    $head = & git -C $rtk rev-parse HEAD
    if ($LASTEXITCODE -ne 0 -or $head -ne $revision) { throw 'RTK HEAD does not match the pinned revision' }
} else {
    $stage = "$rtk.tmp.$([guid]::NewGuid().ToString('N'))"
    & git init --quiet $stage
    if ($LASTEXITCODE -ne 0) { throw 'RTK git init failed' }
    & git -C $stage config core.autocrlf false
    & git -C $stage fetch --quiet --depth 1 $repository $revision
    if ($LASTEXITCODE -ne 0) { throw "RTK fetch failed; incomplete checkout retained at $stage" }
    & git -C $stage checkout --quiet --detach FETCH_HEAD
    if ($LASTEXITCODE -ne 0) { throw 'RTK checkout failed' }
    foreach ($patch in $patches) {
        $patchFile = Join-Path $stage '.tokenless-apply.patch'
        $patchText = [IO.File]::ReadAllText((Join-Path $component "third_party/patches/$patch")).Replace("`r`n", "`n")
        [IO.File]::WriteAllText($patchFile, $patchText, [Text.UTF8Encoding]::new($false))
        & git -C $stage apply --ignore-space-change $patchFile
        if ($LASTEXITCODE -ne 0) { throw "Failed applying $patch; checkout retained at $stage" }
        Remove-Item -LiteralPath $patchFile
    }
    [IO.File]::WriteAllText((Join-Path $stage '.anolisa-rtk-commit'), "$revision`n")
    [IO.File]::WriteAllText((Join-Path $stage '.anolisa-rtk-patches'), "$patchDigest`n")
    [IO.File]::WriteAllText((Join-Path $stage '.anolisa-rtk-tree'), "$(Get-RtkTreeDigest $stage)`n")
    Move-Item -LiteralPath $stage -Destination $rtk
}
$buildArgs = @('+1.91.0', 'build', '--locked', '--target', 'x86_64-pc-windows-msvc')
$profile = 'debug'
if (!$Debug) { $buildArgs += '--release'; $profile = 'release' }
$componentTarget = Get-CargoTargetDirectory (Join-Path $component 'Cargo.toml')
& cargo @buildArgs --manifest-path (Join-Path $component 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'Tokenless build failed' }
# npm packaging consumes this fixed staging path even when Cargo output is redirected.
$output = Join-Path $component 'target/npm-prebuilt/win32-x64'
New-Item -ItemType Directory -Path $output -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $componentTarget "x86_64-pc-windows-msvc/$profile/tokenless.exe") -Destination $output
Write-Output "Windows binaries: $output"
