# 构建 Velmira 桌面应用（MyGo / Go）
# 用法：powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-desktop.ps1 [-Release]
param(
  [switch]$Release
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$go = $null
$fromPath = Get-Command go -ErrorAction SilentlyContinue
if ($fromPath) { $go = $fromPath.Source }
if (-not $go -and (Test-Path (Join-Path $root '.toolchains/go/bin/go.exe'))) {
  $go = Join-Path $root '.toolchains/go/bin/go.exe'
}
if (-not $go) { throw '找不到 Go：请安装 Go 1.27+ 或解压到 .toolchains/go（https://go.dev/dl/）' }

# MyGo 依赖走可用的模块代理；默认 proxy.golang.org 在部分网络不可达
if (-not $env:GOPROXY -or $env:GOPROXY -eq 'https://proxy.golang.org,direct') {
  $env:GOPROXY = 'https://goproxy.cn,direct'
}
$env:CGO_ENABLED = '0'

Push-Location (Join-Path $root 'desktop')
try {
  & $go build -trimpath -ldflags '-s -w' -o Velmira.exe .
  if ($LASTEXITCODE -ne 0) { throw "go build 失败（exit $LASTEXITCODE）" }
  $exe = Get-Item Velmira.exe
  Write-Host ("构建完成：" + $exe.FullName + "（" + [math]::Round($exe.Length / 1MB, 1) + " MB）")
} finally {
  Pop-Location
}
