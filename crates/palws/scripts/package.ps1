param(
    [string]$OutputDirectory = "artifacts",
    [string]$BuildId = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
$crateRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

if ([string]::IsNullOrWhiteSpace($BuildId)) {
    $commitDate = (git -C $repoRoot show -s --format=%cs HEAD).Trim().Replace("-", "")
    $shortSha = (git -C $repoRoot rev-parse --short=7 HEAD).Trim()
    $BuildId = "$commitDate-$shortSha"
}

if (-not $SkipBuild) {
    & cargo build --manifest-path (Join-Path $repoRoot "Cargo.toml") --release -p palws
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }
}

$dllPath = Join-Path $repoRoot "target\release\palws.dll"
$luaPath = Join-Path $crateRoot "lua\main.lua"
if (-not (Test-Path -LiteralPath $dllPath)) {
    throw "Missing build output: $dllPath"
}
if (-not (Test-Path -LiteralPath $luaPath)) {
    throw "Missing Lua entrypoint: $luaPath"
}

if (-not [IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory = Join-Path $repoRoot $OutputDirectory
}
[IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null

$archiveName = "palws-dev-$BuildId.zip"
$archivePath = Join-Path $OutputDirectory $archiveName
$checksumPath = "$archivePath.sha256"
$stageRoot = Join-Path ([IO.Path]::GetTempPath()) ("palws-package-" + [Guid]::NewGuid().ToString("N"))
$modRoot = Join-Path $stageRoot "Palws"
$scriptsRoot = Join-Path $modRoot "Scripts"

try {
    [IO.Directory]::CreateDirectory($scriptsRoot) | Out-Null
    [IO.File]::WriteAllText((Join-Path $modRoot "enabled.txt"), "")
    Copy-Item -LiteralPath $dllPath -Destination (Join-Path $scriptsRoot "palws.dll")
    Copy-Item -LiteralPath $luaPath -Destination (Join-Path $scriptsRoot "main.lua")

    $buildInfo = @(
        "Palws development build"
        "Build: $BuildId"
        "Commit: $((git -C $repoRoot rev-parse HEAD).Trim())"
        "Repository: https://github.com/AzurIce/pal-companion"
    ) -join [Environment]::NewLine
    [IO.File]::WriteAllText((Join-Path $modRoot "BUILD.txt"), $buildInfo + [Environment]::NewLine)

    if (Test-Path -LiteralPath $archivePath) {
        Remove-Item -LiteralPath $archivePath -Force
    }
    Compress-Archive -LiteralPath $modRoot -DestinationPath $archivePath -CompressionLevel Optimal

    $hash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText($checksumPath, "$hash  $archiveName`n")
}
finally {
    if (Test-Path -LiteralPath $stageRoot) {
        Remove-Item -LiteralPath $stageRoot -Recurse -Force
    }
}

Write-Output "archive=$archivePath"
Write-Output "checksum=$checksumPath"
