<#
.SYNOPSIS
  M0 一键回归：起隔离服务 -> 跑 A-F 断言（tests/verify_m0.py）-> 静态检查 -> 单测 -> 停服务清理。
  只写 tests/ 与临时 data 目录，不改 server/ web/ docs/。
.EXAMPLE
  pwsh -File scripts/verify.ps1
  pwsh -File scripts/verify.ps1 -Port 8810
#>
param(
  [int]$Port = 8780,
  [string]$BookFile = ""   # 留空 = 自动取 book/ 下第一本 txt，不在脚本里写死书名
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
if (-not $BookFile) {
  $firstBook = Get-ChildItem (Join-Path $root "book") -Filter *.txt -File | Select-Object -First 1
  if (-not $firstBook) { throw "book/ 下没有 txt 测试书" }
  $BookFile = "book/" + $firstBook.Name
}
$rand = Get-Random -Maximum 999999
$dataDir = Join-Path $root ("data/_test_verify_" + $rand)
$logPath = Join-Path $root "tests/verify_server.log"
$server = $null
$failures = @()

function Step($name) { Write-Host ""; Write-Host "=== $name ===" -ForegroundColor Cyan }

try {
  Step "0. 冷启动前置检查"
  if (Test-Path (Join-Path $root "data")) {
    $existing = Get-ChildItem (Join-Path $root "data") -Force -ErrorAction SilentlyContinue
    if ($existing) { Write-Host ("data/ 非空，仍用隔离目录: " + $dataDir) } else { Write-Host "data/ 为空（冷启动）" }
  } else { Write-Host "data/ 不存在（冷启动）" }
  $busy = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
  if ($busy) { throw "端口 $Port 已被占用（pid=" + ($busy.OwningProcess -join ',') + "），换 -Port" }

  Step "1. 起服务（DATA_DIR=$dataDir, port=$Port）"
  $env:DATA_DIR = $dataDir
  $env:PYTHONIOENCODING = 'utf-8'
  $server = Start-Process -FilePath "python" -ArgumentList "-m","uvicorn","server.app.main:app","--host","127.0.0.1","--port","$Port" -WorkingDirectory $root -PassThru -RedirectStandardOutput $logPath -RedirectStandardError ($logPath + ".err") -WindowStyle Hidden
  $ready = $false
  for ($i = 0; $i -lt 80; $i++) {
    Start-Sleep -Milliseconds 250
    try {
      $h = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/health" -TimeoutSec 2
      if ($h.ok -and $h.version -match '^0\.\d+\.0$') { $ready = $true; Write-Host ("health OK: " + ($h | ConvertTo-Json -Compress)); break }
    } catch { }
  }
  if (-not $ready) { throw "服务未就绪，见 $logPath" }

  Step "2. A-F API 断言（tests/verify_m0.py）"
  python tests/verify_m0.py --base "http://127.0.0.1:$Port"
  if ($LASTEXITCODE -ne 0) { $failures += "verify_m0.py 存在 FAIL（exit $LASTEXITCODE）" }

  Step "3. F 前端静态检查"
  $tmp = Join-Path $env:TEMP ("webjs_" + (Get-Random))
  New-Item -ItemType Directory -Path $tmp -Force | Out-Null
  $jsFail = 0
  $jsFiles = Get-ChildItem web -Recurse -Filter *.js
  foreach ($f in $jsFiles) {
    $rel = $f.FullName.Substring($root.Length + 1)
    $dest = Join-Path $tmp (($rel -replace '[/\\]', '_') -replace '\.js$', '.mjs')
    Copy-Item $f.FullName $dest
    & node --check $dest 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { $jsFail++; Write-Host ("node --check FAIL: " + $rel) -ForegroundColor Red }
  }
  Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
  $totalJs = ($jsFiles | Measure-Object -Property Length -Sum).Sum
  Write-Host ("node --check: " + $jsFiles.Count + " files, fail=" + $jsFail)
  Write-Host ("JS total: " + $totalJs + " bytes (" + [math]::Round($totalJs / 1024, 1) + " KB), budget 150KB")
  if ($jsFail -gt 0) { $failures += "node --check 失败 $jsFail 个文件" }
  if ($totalJs -ge 150 * 1024) { $failures += "JS 体积超 150KB" }

  Step "4. H 单测"
  python -m unittest discover -s tests -v
  if ($LASTEXITCODE -ne 0) { $failures += "unittest 失败" }

  Step "5. 收尾"
}
finally {
  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
  }
  if (Test-Path $dataDir) { Remove-Item $dataDir -Recurse -Force -ErrorAction SilentlyContinue }
  $left = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
  if ($left) { Write-Host ("警告：端口 $Port 仍有监听 pid=" + ($left.OwningProcess -join ',')) -ForegroundColor Yellow }
  else { Write-Host ("端口 $Port 已释放，数据目录已删除") }
}

Write-Host ""
if ($failures.Count -eq 0) {
  Write-Host "M0 验收回归：全部 PASS" -ForegroundColor Green
  exit 0
} else {
  Write-Host "M0 验收回归：存在失败项：" -ForegroundColor Red
  $failures | ForEach-Object { Write-Host (" - " + $_) -ForegroundColor Red }
  exit 1
}
