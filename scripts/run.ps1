# Velmira 开发服务器启动脚本（Lead 维护）
param(
  [int]$Port = 8765,
  [switch]$Reload,
  [string]$DataDir = ""
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

if ($DataDir -ne "") {
  $env:DATA_DIR = $DataDir
}
Write-Host "Velmira v0.3.0"
Write-Host "[run] root      = $Root"
Write-Host "[run] data dir  = $(if ($env:DATA_DIR) { $env:DATA_DIR } else { Join-Path $Root 'data' })"
Write-Host "[run] url       = http://127.0.0.1:$Port/"

$args = @("-m", "uvicorn", "server.app.main:app", "--host", "127.0.0.1", "--port", "$Port")
if ($Reload) { $args += "--reload" }
python @args
