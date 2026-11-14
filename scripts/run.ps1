# Velmira 开发服务器启动脚本
param(
  [int]$Port = 8765,
  [switch]$Reload,
  [string]$DataDir = "",
  [string]$BindAddress = "127.0.0.1"   # 手机等局域网设备访问时用 0.0.0.0
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

if ($DataDir -ne "") {
  $env:DATA_DIR = $DataDir
}
Write-Host "Velmira v0.5.3"
Write-Host "[run] root      = $Root"
Write-Host "[run] data dir  = $(if ($env:DATA_DIR) { $env:DATA_DIR } else { Join-Path $Root 'data' })"
Write-Host "[run] bind      = $BindAddress"
Write-Host "[run] url       = http://${BindAddress}:$Port/  (本机 http://127.0.0.1:$Port/)"

$args = @("-m", "uvicorn", "server.app.main:app", "--host", $BindAddress, "--port", "$Port", "--no-proxy-headers")
if ($Reload) { $args += "--reload" }
python @args
