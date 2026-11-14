<#
.SYNOPSIS
  M1（Velmira v0.5.2）一键独立验收：
  冷启动隔离服务 -> TXT M0 回归(41) -> M1 API(A/C/D/E/F) -> 品牌/体积/node --check ->
  单测(37) -> 重启持久化 -> CDP 前端实测(G) -> 旧库升级(I) -> 清理。
  只写 tests/ 与 scripts/，不改 server/ web/ docs/ book/。
.EXAMPLE
  powershell -NoProfile -File scripts/verify_m1.ps1
  powershell -NoProfile -File scripts/verify_m1.ps1 -Port 8787 -CdpPort 9334 -SkipCdp
#>
param(
  [int]$Port = 8787,
  [int]$CdpPort = 9334,
  [string]$BookFile = "",   # 留空 = 自动取 book/ 下第一本 txt，不在脚本里写死书名
  [switch]$SkipCdp
)
# PS 5.1 下原生命令（python/node 把进度点写到 stderr）会触发 Stop，这里统一 Continue；
# 关键失败都有显式 if ($LASTEXITCODE) / throw 检查兜底，不会静默通过。
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
if (-not $BookFile) {
  $firstBook = Get-ChildItem (Join-Path $root "book") -Filter *.txt -File | Select-Object -First 1
  if (-not $firstBook) { throw "book/ 下没有 txt 测试书" }
  $BookFile = "book/" + $firstBook.Name
}
$rand = Get-Random -Maximum 999999
$dataDir = Join-Path $root ("data/_m1_verify_" + $rand)
$env:DATA_DIR = $dataDir
$env:PYTHONIOENCODING = 'utf-8'
$server = $null
$failures = @()
function Step($name) { Write-Host ""; Write-Host "=== $name ===" -ForegroundColor Cyan }
function Start-Server([int]$p, [string]$logName) {
  $log = Join-Path $root ("tests/" + $logName)
  $proc = Start-Process -FilePath "python" -ArgumentList "-m","uvicorn","server.app.main:app","--host","127.0.0.1","--port","$p","--no-proxy-headers" -WorkingDirectory $root -PassThru -RedirectStandardOutput $log -RedirectStandardError ($log + ".err") -WindowStyle Hidden
  for ($i = 0; $i -lt 80; $i++) {
    Start-Sleep -Milliseconds 250
    try { $h = Invoke-RestMethod -Uri "http://127.0.0.1:$p/api/health" -TimeoutSec 2; if ($h.ok) { return @{ proc = $proc; health = $h } } } catch { }
  }
  throw "服务未就绪（端口 $p），见 $log"
}
function Stop-Server($proc) {
  if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 700 }
}
try {
  Step "0. 前置"
  $busy = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
  if ($busy) { throw "端口 $Port 被占用（pid=" + ($busy.OwningProcess -join ',') + "）" }
  Write-Host ("隔离 DATA_DIR=" + $dataDir)

  Step "1. 冷启动 + 健康/品牌"
  $started = Start-Server $Port "m1_verify_server.log"
  $server = $started.proc
  Write-Host ("health=" + ($started.health | ConvertTo-Json -Compress))
  if ($started.health.name -ne "Velmira" -or $started.health.version -ne "0.5.2") { $failures += "健康检查 name/version 不符" }

  Step "2. TXT M0 回归（tests/verify_m0.py，版本断言已更新 0.5.2）"
  python tests/verify_m0.py --base "http://127.0.0.1:$Port" 2>&1 | Select-Object -Last 4
  if ($LASTEXITCODE -ne 0) { $failures += "verify_m0.py FAIL（exit $LASTEXITCODE）" }

  Step "3. M1 API：EPUB/CBZ/注释/书签（tests/verify_m1.py --phase api）"
  python tests/verify_m1.py --base "http://127.0.0.1:$Port" --phase api 2>&1 | Select-Object -Last 8
  if ($LASTEXITCODE -ne 0) { $failures += "verify_m1.py api FAIL（exit $LASTEXITCODE）" }

  Step "4. 品牌静态 + 体积 + node --check"
  $codeFiles = Get-ChildItem web,server -Recurse -File -Include *.js,*.css,*.html,*.py
  $legacyPattern = 'AI 阅读\+|Ai Reading Plus|\bARP\b'
  $oldHits = @($codeFiles | Select-String -Pattern $legacyPattern)
  $readmeHits = @(Select-String -Path README.md -Pattern $legacyPattern -ErrorAction SilentlyContinue)
  $docHits = @(Get-ChildItem web,server -Recurse -File -Filter *.md -ErrorAction SilentlyContinue | Select-String -Pattern $legacyPattern)
  Write-Host ("旧名命中：代码/README=" + ($oldHits.Count + $readmeHits.Count) + "，文档(md,历史记录)=" + $docHits.Count)
  $oldHits | ForEach-Object { Write-Host ("  " + $_.Path + ":" + $_.LineNumber) }
  if (($oldHits.Count + $readmeHits.Count) -gt 0) { $failures += "web/server/README 仍有旧品牌名（历史名）" }
  $jsFiles = Get-ChildItem web -Recurse -Filter *.js
  $tmp = Join-Path $env:TEMP ("m1js_" + (Get-Random)); New-Item -ItemType Directory -Path $tmp -Force | Out-Null
  $bad = 0
  foreach ($f in $jsFiles) {
    $dest = Join-Path $tmp (($f.Name) -replace '\.js$', '.mjs'); Copy-Item $f.FullName $dest
    node --check $dest 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { $bad++; Write-Host ("node --check FAIL " + $f.Name) -ForegroundColor Red }
  }
  Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
  $jsTotal = ($jsFiles | Measure-Object -Property Length -Sum).Sum
  $cssTotal = (Get-ChildItem web -Recurse -Filter *.css | Measure-Object -Property Length -Sum).Sum
  Write-Host ("JS=" + $jsTotal + " B (" + [math]::Round($jsTotal / 1024, 1) + " KB)/" + $jsFiles.Count + " 文件；CSS=" + $cssTotal + " B；index.html=" + (Get-Item web/index.html).Length + " B；node fail=" + $bad)
  if ($bad -gt 0) { $failures += "node --check 失败 $bad" }
  # Includes the optional settings and offline modules in the unminified total.
  if ($jsTotal -ge 400 * 1024) { $failures += "JS 总量超 400KB" }
  if (-not (Select-String -Path web/ui/library.js -Pattern 'fmt-badge' -SimpleMatch -Quiet)) { $failures += "书库未发现格式徽标实现" }

  Step "5. 单测（37）"
  python -m unittest discover -s tests 2>&1 | Select-Object -Last 3
  if ($LASTEXITCODE -ne 0) { $failures += "unittest FAIL" }

  Step "6. 重启同 DATA_DIR：注释/书签持久化"
  Stop-Server $server
  $started = Start-Server $Port "m1_persist_server.log"
  $server = $started.proc
  python tests/verify_m1.py --base "http://127.0.0.1:$Port" --phase persist 2>&1 | Select-Object -Last 5
  if ($LASTEXITCODE -ne 0) { $failures += "持久化验证 FAIL（注意 CDP 会清理书签，需在 --phase api 后立即跑）" }

  Step "7. 前端 CDP 实测（G）"
  if (-not $SkipCdp) {
    node tests/cdp_m1.mjs --base "http://127.0.0.1:$Port" --port $CdpPort 2>&1 | Select-Object -Last 5
    if ($LASTEXITCODE -ne 0) { $failures += "CDP 前端验收 FAIL（exit $LASTEXITCODE）" }
  } else { Write-Host "已跳过 CDP" }

  Step "8. 旧库升级（只读复制 data/app.db）"
  $oldDb = Join-Path $root "data/app.db"
  $upData = Join-Path $root ("data/_m1_upgrade_" + (Get-Random -Maximum 999999))
  if (Test-Path $oldDb) {
    $hash0 = (Get-FileHash $oldDb -Algorithm SHA256).Hash
    New-Item -ItemType Directory -Path $upData -Force | Out-Null
    Copy-Item $oldDb (Join-Path $upData "app.db") -Force
    Stop-Server $server
    $env:DATA_DIR = $upData
    $up = Start-Server $Port "m1_upgrade_server.log"
    $server = $up.proc
    $books = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/books"
    $bid = @($books.books)[0].id
    $toc = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/books/$bid/toc"
    Write-Host ("旧库书籍=" + @($books.books).Count + " 首本=" + $bid + " TOC=" + @($toc.chapters).Count)
    Stop-Server $server
    python tests/_m1_upgrade_check.py $upData 2>&1 | Select-Object -Last 4
    if ($LASTEXITCODE -ne 0) { $failures += "旧库 init_db 幂等/新表检查 FAIL" }
    $env:DATA_DIR = $dataDir
    $hash1 = (Get-FileHash $oldDb -Algorithm SHA256).Hash
    Write-Host ("data/app.db 未被修改=" + ($hash0 -eq $hash1))
    if ($hash0 -ne $hash1) { $failures += "data/app.db 被改动" }
    Remove-Item $upData -Recurse -Force -ErrorAction SilentlyContinue
  } else { Write-Host "data/app.db 不存在，跳过升级验证" }
}
finally {
  Stop-Server $server
  if (Test-Path $dataDir) { Remove-Item $dataDir -Recurse -Force -ErrorAction SilentlyContinue }
  $left = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
  if ($left) { Write-Host ("警告：端口 $Port 仍有监听") -ForegroundColor Yellow } else { Write-Host ("端口 $Port 已释放，数据目录已删除") }
}
Write-Host ""
if ($failures.Count -eq 0) { Write-Host "M1 验收回归：全部 PASS" -ForegroundColor Green; exit 0 }
else { Write-Host "M1 验收回归：存在失败项：" -ForegroundColor Red; $failures | ForEach-Object { Write-Host (" - " + $_) -ForegroundColor Red }; exit 1 }
