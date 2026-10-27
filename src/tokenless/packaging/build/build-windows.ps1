param([switch]$Debug)
$ErrorActionPreference = 'Stop'
$component = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$rtk = Join-Path $component 'third_party/rtk'
function Get-CargoTargetDirectory([string]$Manifest) {
    # Cargo resolves build.target-dir too; PS 5.1 needs UTF-8 decoding (PS 7 defaults to it).
    $previousEncoding = [Console]::OutputEncoding
    try {
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        $metadata = & cargo metadata --format-version 1 --no-deps --locked --manifest-path $Manifest
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
# The existing setup script remains the single source of the pinned revision.
$source = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'setup-rtk.sh') -Raw
$revision = [regex]::Match($source, '(?m)^RTK_COMMIT="([0-9a-f]{40})"').Groups[1].Value
$repository = [regex]::Match($source, '(?m)^RTK_REPOSITORY="([^"]+)"').Groups[1].Value
if (!$revision -or !$repository) { throw 'RTK source pin is missing from setup-rtk.sh' }
if (Test-Path -LiteralPath $rtk) {
    $marker = Get-Content -LiteralPath (Join-Path $rtk '.anolisa-rtk-commit') -Raw
    if ($marker.Trim() -ne $revision) { throw 'RTK checkout does not match the pinned revision' }
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
    foreach ($patch in @('rtk-tokenless-stats.patch', 'rtk-pytest-error-report.patch')) {
        $patchFile = Join-Path $stage '.tokenless-apply.patch'
        $patchText = [IO.File]::ReadAllText((Join-Path $component "third_party/patches/$patch")).Replace("`r`n", "`n")
        [IO.File]::WriteAllText($patchFile, $patchText, [Text.UTF8Encoding]::new($false))
        & git -C $stage apply --ignore-space-change $patchFile
        if ($LASTEXITCODE -ne 0) { throw "Failed applying $patch; checkout retained at $stage" }
        Remove-Item -LiteralPath $patchFile
    }
    [IO.File]::WriteAllText((Join-Path $stage '.anolisa-rtk-commit'), "$revision`n")
    Move-Item -LiteralPath $stage -Destination $rtk
}
$buildArgs = @('build', '--locked', '--target', 'x86_64-pc-windows-msvc')
$profile = 'debug'
if (!$Debug) { $buildArgs += '--release'; $profile = 'release' }
$componentTarget = Get-CargoTargetDirectory (Join-Path $component 'Cargo.toml')
$rtkTarget = Get-CargoTargetDirectory (Join-Path $rtk 'Cargo.toml')
& cargo @buildArgs --manifest-path (Join-Path $component 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'Tokenless build failed' }
& cargo @buildArgs --manifest-path (Join-Path $rtk 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'RTK build failed' }
# npm packaging consumes this fixed staging path even when Cargo output is redirected.
$output = Join-Path $component 'target/npm-prebuilt/win32-x64'
New-Item -ItemType Directory -Path $output -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $componentTarget "x86_64-pc-windows-msvc/$profile/tokenless.exe") -Destination $output
Copy-Item -LiteralPath (Join-Path $rtkTarget "x86_64-pc-windows-msvc/$profile/rtk.exe") -Destination $output
Write-Output "Windows binaries: $output"
