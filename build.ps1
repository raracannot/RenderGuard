# 构建便携版 RenderGuard.exe

$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
$src = Join-Path $here "src-tauri"

Write-Host "==> 编译 release（首次较慢）" -ForegroundColor Cyan
Push-Location $src
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build 失败" }
} finally {
    Pop-Location
}

$exe = Join-Path $src "target\release\renderguard.exe"
if (-not (Test-Path $exe)) { throw "没找到产物: $exe" }

$dist = Join-Path $here "dist"
New-Item -ItemType Directory -Force -Path $dist | Out-Null
Copy-Item $exe (Join-Path $dist "RenderGuard.exe") -Force

# 便携模式标记：数据写在 exe 同级的 data\ 里，不碰 %LOCALAPPDATA%
New-Item -ItemType File -Force -Path (Join-Path $dist "portable.txt") | Out-Null
if (Test-Path (Join-Path $here "README.md")) {
    Copy-Item (Join-Path $here "README.md") $dist -Force
}

$size = [math]::Round((Get-Item (Join-Path $dist "RenderGuard.exe")).Length / 1MB, 1)
Write-Host ""
Write-Host "完成: $dist\RenderGuard.exe  ($size MB)" -ForegroundColor Green
Write-Host "便携模式已开启（portable.txt）→ 数据保存在 $dist\data\" -ForegroundColor DarkGray
