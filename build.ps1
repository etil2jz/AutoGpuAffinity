# Compile AutoGpuAffinity et assemble le dossier de publication dans .\build\AutoGpuAffinity\
$ErrorActionPreference = "Stop"

Push-Location $PSScriptRoot
try {
    if (Test-Path ".\build\") {
        Remove-Item -Path ".\build\" -Recurse -Force
    }

    cargo build --release --locked -p autogpuaffinity
    if ($LASTEXITCODE -ne 0) {
        Write-Host "error: cargo build failed"
        exit $LASTEXITCODE
    }

    $package = ".\build\AutoGpuAffinity"
    New-Item -ItemType Directory -Path $package -Force | Out-Null
    Copy-Item ".\target\release\autogpuaffinity.exe" "$package\AutoGpuAffinity.exe"
    Copy-Item ".\AutoGpuAffinity\bin" $package -Recurse
    Copy-Item ".\AutoGpuAffinity\config.ini" $package

    Write-Host "package created in $package"
}
finally {
    Pop-Location
}
