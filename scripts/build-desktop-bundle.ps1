# 构建 Velmira 自包含桌面分发包：MyGo 窗口 + PyInstaller 后端边车。
# 目标机器无需另装 Python；数据保存在 %LOCALAPPDATA%\Velmira\data。
param(
  [string]$Python = "",
  [string]$Out = "dist\Velmira-desktop-v0.4.0"
)

# PS5.1 原生命令将诊断写到 stderr；每步以 LASTEXITCODE 检查结果。
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $Python) { $Python = $env:VELMIRA_PYTHON }
if (-not $Python) {
  $found = Get-Command python -ErrorAction SilentlyContinue
  if ($found) { $Python = $found.Source }
}
if (-not $Python) { throw '找不到 Python 3.11：请用 -Python 指定可执行文件' }

& $Python -c 'import fastapi,uvicorn,PIL,PyInstaller' 2>$null
if ($LASTEXITCODE -ne 0) {
  throw "Python 环境缺少 fastapi/uvicorn/Pillow/PyInstaller；先运行 pip install -r server/requirements.txt pyinstaller，或用 -Python 指向已配置环境"
}

$bundle = if ([IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $root $Out }
$bundle = [IO.Path]::GetFullPath($bundle)
$distRoot = [IO.Path]::GetFullPath((Join-Path $root 'dist')) + [IO.Path]::DirectorySeparatorChar
if (-not $bundle.StartsWith($distRoot, [StringComparison]::OrdinalIgnoreCase)) {
  throw '分发目录必须位于本项目 dist/ 下，避免误覆盖其他目录'
}
New-Item -ItemType Directory -Force -Path (Join-Path $root 'backend') | Out-Null
$backendDir = Join-Path $root 'backend\Velmira-backend'
$backendOneFile = Join-Path $root 'backend\Velmira-backend.exe'
if (Test-Path -LiteralPath $backendDir) { Remove-Item -LiteralPath $backendDir -Recurse -Force }
if (Test-Path -LiteralPath $backendOneFile) { Remove-Item -LiteralPath $backendOneFile -Force }

$previousPythonPath = $env:PYTHONPATH
$env:PYTHONPATH = if ($previousPythonPath) { $root + ';' + $previousPythonPath } else { $root }
try {
  # Directory mode avoids PyInstaller extracting a 35 MB archive on every launch.
  & $Python -m PyInstaller --noconfirm --clean --onedir `
  --name Velmira-backend `
  --distpath (Join-Path $root 'backend') `
  --workpath (Join-Path $root '.tmp\pyi-work') `
  --specpath (Join-Path $root '.tmp') `
  --paths $root `
  --add-data ($root + '\web;web') `
  --add-data ($root + '\server\app\schema.sql;server/app') `
  --add-data ($root + '\server\app\manga\face_worker.py;server/app/manga') `
    (Join-Path $root 'server\launcher.py')
  if ($LASTEXITCODE -ne 0) { throw 'PyInstaller 构建后端失败' }
} finally {
  $env:PYTHONPATH = $previousPythonPath
}

$buildScript = Join-Path $root 'scripts\build-desktop.ps1'
& powershell -NoProfile -ExecutionPolicy Bypass -File $buildScript
if ($LASTEXITCODE -ne 0) { throw 'MyGo 桌面壳构建失败' }

$ErrorActionPreference = 'Stop'
if (Test-Path -LiteralPath $bundle) { Remove-Item -LiteralPath $bundle -Recurse -Force }
New-Item -ItemType Directory -Force -Path $bundle | Out-Null
Copy-Item (Join-Path $root 'desktop\Velmira.exe') (Join-Path $bundle 'Velmira.exe') -Force
New-Item -ItemType Directory -Force -Path (Join-Path $bundle 'backend') | Out-Null
Copy-Item $backendDir (Join-Path $bundle 'backend\Velmira-backend') -Recurse -Force
foreach ($name in @('README.md','LICENSE','NOTICE.md')) {
  if (Test-Path (Join-Path $root $name)) { Copy-Item (Join-Path $root $name) (Join-Path $bundle $name) -Force }
}
Copy-Item (Join-Path $root 'desktop\START-HERE.txt') (Join-Path $bundle 'START-HERE.txt') -Force
& $Python (Join-Path $root 'scripts\collect-bundle-licenses.py') --bundle $bundle --root $root
if ($LASTEXITCODE -ne 0) { throw '收集分发依赖许可失败' }
$exe = Get-Item (Join-Path $bundle 'Velmira.exe')
$backend = Get-ChildItem (Join-Path $bundle 'backend\Velmira-backend') -File -Recurse | Measure-Object -Property Length -Sum
Write-Host ("桌面分发包：" + $bundle)
Write-Host ("  Velmira.exe=" + [math]::Round($exe.Length / 1MB, 1) + " MB；backend=" + [math]::Round($backend.Sum / 1MB, 1) + " MB（目录模式）")
