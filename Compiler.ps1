$ErrorActionPreference = 'Stop'
Push-Location -LiteralPath $PSScriptRoot
try {
    & cargo test --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Les tests ont échoué.' }
    & cargo clippy --release --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'La vérification Clippy a échoué.' }
    & cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'La compilation a échoué.' }
    $dist = Join-Path $PSScriptRoot 'dist'
    New-Item -ItemType Directory -Path $dist -Force | Out-Null
    $exe = Join-Path $dist 'TokenScope.exe'
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\tokenscope.exe') -Destination $exe -Force
    $hash = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
    Set-Content -LiteralPath (Join-Path $dist 'SHA256SUMS.txt') -Value "$hash  TokenScope.exe" -Encoding ascii
    Write-Output 'dist\TokenScope.exe et son empreinte SHA-256 sont prêts.'
} finally {
    Pop-Location
}

