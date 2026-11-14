# 构建 Velmira 自包含桌面分发包：MyGo 窗口 + PyInstaller 后端边车。
# 目标机器无需另装 Python；数据保存在 %LOCALAPPDATA%\Velmira\data。
param(
  [string]$Python = "",
  [string]$Out = "dist\Velmira-desktop-v0.5.3"
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

& $Python -c 'import fastapi,uvicorn,PIL,qrcode,tiktoken,PyInstaller' 2>$null
if ($LASTEXITCODE -ne 0) {
  throw "Python 环境缺少 fastapi/uvicorn/Pillow/qrcode/tiktoken/PyInstaller；先运行 pip install -r server/requirements.txt pyinstaller，或用 -Python 指向已配置环境"
}

$bundle = if ([IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $root $Out }
$bundle = [IO.Path]::GetFullPath($bundle)
$distRoot = [IO.Path]::GetFullPath((Join-Path $root 'dist')) + [IO.Path]::DirectorySeparatorChar
if (-not $bundle.StartsWith($distRoot, [StringComparison]::OrdinalIgnoreCase)) {
  throw '分发目录必须位于本项目 dist/ 下，避免误覆盖其他目录'
}
$backendBuildRoot = Join-Path $root ('.tmp\backend-dist-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $backendBuildRoot | Out-Null
$tokenCache = Join-Path $root '.tmp\tiktoken-cache'
New-Item -ItemType Directory -Force -Path $tokenCache | Out-Null
$existingTokenCache = if ($env:TIKTOKEN_CACHE_DIR) { $env:TIKTOKEN_CACHE_DIR } else { Join-Path ([IO.Path]::GetTempPath()) 'data-gym-cache' }
if ((Test-Path $existingTokenCache) -and ([IO.Path]::GetFullPath($existingTokenCache) -ne [IO.Path]::GetFullPath($tokenCache))) {
  Get-ChildItem $existingTokenCache -File | Copy-Item -Destination $tokenCache -Force
}
$oldTokenCache = $env:TIKTOKEN_CACHE_DIR
$env:TIKTOKEN_CACHE_DIR = $tokenCache
try {
  # Public vocabulary files are bundled so generation never needs to fetch a
  # tokenizer on the target machine. These downloads are build dependencies.
  $prepareTokens = Join-Path $root '.tmp\prepare-tokenizer.py'
  @'
import tiktoken
tiktoken.get_encoding("cl100k_base")
tiktoken.get_encoding("o200k_base")
'@ | Set-Content -Encoding UTF8 $prepareTokens
  & $Python $prepareTokens
  if ($LASTEXITCODE -ne 0) { throw '准备离线 tokenizer 词表失败' }
} finally { $env:TIKTOKEN_CACHE_DIR = $oldTokenCache }
$backendDir = Join-Path $backendBuildRoot 'Velmira-backend'

$previousPythonPath = $env:PYTHONPATH
$env:PYTHONPATH = if ($previousPythonPath) { $root + ';' + $previousPythonPath } else { $root }
try {
  # Directory mode avoids PyInstaller extracting a 35 MB archive on every launch.
  & $Python -m PyInstaller --noconfirm --clean --onedir `
  --name Velmira-backend `
  --distpath $backendBuildRoot `
  --workpath (Join-Path $root '.tmp\pyi-work') `
  --specpath (Join-Path $root '.tmp') `
  --paths $root `
  --collect-submodules qrcode `
  --collect-submodules tiktoken_ext `
  --add-data ($tokenCache + ';tiktoken-cache') `
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
# The runtime package is not the source repository: ship the runtime-specific
# readme and the docs that README.md links to, so the first run is not a
# scavenger hunt for files that only exist in Git.
Copy-Item (Join-Path $root 'desktop\README-DIST.md') (Join-Path $bundle 'README-DIST.md') -Force
if (Test-Path (Join-Path $root 'docs')) {
  Copy-Item (Join-Path $root 'docs') (Join-Path $bundle 'docs') -Recurse -Force
}
Copy-Item (Join-Path $root 'desktop\START-HERE.txt') (Join-Path $bundle 'START-HERE.txt') -Force
& $Python (Join-Path $root 'scripts\collect-bundle-licenses.py') --bundle $bundle --root $root
if ($LASTEXITCODE -ne 0) { throw '收集分发依赖许可失败' }
$exe = Get-Item (Join-Path $bundle 'Velmira.exe')
$backend = Get-ChildItem (Join-Path $bundle 'backend\Velmira-backend') -File -Recurse | Measure-Object -Property Length -Sum
Write-Host ("桌面分发包：" + $bundle)
Write-Host ("  Velmira.exe=" + [math]::Round($exe.Length / 1MB, 1) + " MB；backend=" + [math]::Round($backend.Sum / 1MB, 1) + " MB（目录模式）")
