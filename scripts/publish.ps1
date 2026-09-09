# publish.ps1 — 构建并发布安装包
# 产物复制到 <项目根>/release/（所有历史版本保留，不清理；
# 同版本重发=重新发布该版本，release/ 中同名文件会被覆盖，属有意行为）
#
# 用法：
#   ./publish.ps1             # 完整构建（前端 + Rust + NSIS）后发布
#   ./publish.ps1 -SkipBuild  # 跳过构建，仅把最新安装包复制到 release/
#
# 构建环境（本机约定）：
#   - GNU 工具链：.tools\mingw64\bin 与 %USERPROFILE%\.cargo\bin 加入 PATH
#   - 代理：HTTP_PROXY / HTTPS_PROXY 各自独立默认 http://127.0.0.1:10808

param(
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
# 脚本位于 <项目根>\scripts\，项目根取其上一级
$root = Split-Path $PSScriptRoot -Parent
$releaseDir = Join-Path $root 'release'
$nsisDir = Join-Path $root 'src-tauri\target\release\bundle\nsis'

# 构建环境
$mingw = Join-Path $root '.tools\mingw64\bin'
if (Test-Path $mingw) { $env:PATH = "$mingw;$env:PATH" }
$cargo = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path $cargo) { $env:PATH = "$cargo;$env:PATH" }
if (-not $env:HTTP_PROXY) { $env:HTTP_PROXY = 'http://127.0.0.1:10808' }
if (-not $env:HTTPS_PROXY) { $env:HTTPS_PROXY = 'http://127.0.0.1:10808' }

$buildStart = Get-Date

if (-not $SkipBuild) {
    Push-Location $root
    try {
        Write-Host '==> npm run build ...'
        npm run build
        if ($LASTEXITCODE -ne 0) { throw "build failed (exit $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
} else {
    Write-Host '==> skip build'
}

# 只接受本次构建（或 -SkipBuild 时最新）产生的安装包，防止发布陈旧产物
if (-not (Test-Path $nsisDir)) { throw "no NSIS output dir: $nsisDir" }
$candidates = Get-ChildItem (Join-Path $nsisDir 'dsh_shell_*_x64-setup.exe')
if (-not $SkipBuild) {
    $candidates = $candidates | Where-Object { $_.LastWriteTime -ge $buildStart }
}
$pkg = $candidates | Sort-Object LastWriteTime, Name -Descending | Select-Object -First 1
if (-not $pkg) { throw 'no installer produced for this build (bundling/NSIS did not run?)' }

# 复制到 release/（同名覆盖=重新发布该版本；不同版本号自然累积保留）
New-Item -ItemType Directory -Force $releaseDir | Out-Null
Copy-Item $pkg.FullName (Join-Path $releaseDir $pkg.Name) -Force

Write-Host "==> published: $($pkg.Name)"
Get-ChildItem $releaseDir -Filter '*.exe' | Sort-Object Name |
    ForEach-Object { Write-Host ("    {0}  {1:N0} KB" -f $_.Name, ($_.Length / 1KB)) }
