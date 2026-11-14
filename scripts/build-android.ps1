# 构建 Velmira Android 通用 APK（不依赖 Gradle）
# 用法：powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-android.ps1
param(
  [string]$Sdk = "",
  [string]$Out = "dist\Velmira-universal.apk",
  [switch]$Debuggable
)
# 原生命令（javac/d8/keytool/apksigner）把进度写到 stderr，PS5.1 下 Stop 会误判为失败；
# 关键失败都有显式 $LASTEXITCODE / throw 检查兜底，不会静默通过。
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $Sdk) {
  if ($env:ANDROID_HOME) { $Sdk = $env:ANDROID_HOME }
  elseif (Test-Path (Join-Path $root '.toolchains\android\sdk')) { $Sdk = Join-Path $root '.toolchains\android\sdk' }
  else { throw '找不到 Android SDK：安装 cmdline-tools 到 .toolchains/android/sdk，或设置 ANDROID_HOME' }
}
$Sdk = (Resolve-Path $Sdk).Path

$buildTools = Get-ChildItem (Join-Path $Sdk 'build-tools') -Directory | Sort-Object Name -Descending | Select-Object -First 1
if (-not $buildTools) { throw "缺少 build-tools，见 $Sdk\build-tools" }
$platform = Get-ChildItem (Join-Path $Sdk 'platforms') -Directory | Where-Object { $_.Name -match 'android-3[0-9]' } | Sort-Object Name -Descending | Select-Object -First 1
if (-not $platform) { throw "缺少 android platform，见 $Sdk\platforms" }

$aapt2 = Join-Path $buildTools.FullName 'aapt2.exe'
$d8 = Join-Path $buildTools.FullName 'd8.bat'
$zipalign = Join-Path $buildTools.FullName 'zipalign.exe'
$apksigner = Join-Path $buildTools.FullName 'apksigner.bat'
$androidJar = Join-Path $platform.FullName 'android.jar'
foreach ($p in @($aapt2, $d8, $zipalign, $apksigner, $androidJar)) {
  if (-not (Test-Path $p)) { throw "缺少工具：$p" }
}

$app = Join-Path $root 'android\app\src\main'
$work = Join-Path $root 'android\build'
if (Test-Path $work) {
  $resolvedWork = (Resolve-Path -LiteralPath $work).Path.TrimEnd('\')
  $expectedWork = [System.IO.Path]::GetFullPath((Join-Path $root 'android\build')).TrimEnd('\')
  if ($resolvedWork -ne $expectedWork) { throw '构建清理路径不在预期 Android 工作目录内' }
  Remove-Item -LiteralPath $resolvedWork -Recurse -Force
}
New-Item -ItemType Directory -Force -Path (Join-Path $work 'gen'), (Join-Path $work 'classes'), (Join-Path $work 'dex') | Out-Null

# Package only the application shell. Books, API credentials and generated
# media stay outside web/ and are never copied into the APK.
$assetRoot = Join-Path $work 'assets'
$webAssets = Join-Path $assetRoot 'web'
$webSource = (Resolve-Path -LiteralPath (Join-Path $root 'web')).Path
New-Item -ItemType Directory -Force -Path $webAssets | Out-Null
$assetExtensions = @('.html', '.js', '.css', '.webmanifest', '.png', '.svg', '.ico', '.webp')
$assetFiles = @(Get-ChildItem -LiteralPath $webSource -Recurse -File | Where-Object { $_.Extension.ToLowerInvariant() -in $assetExtensions })
foreach ($assetFile in $assetFiles) {
  $relativeAsset = $assetFile.FullName.Substring($webSource.Length).TrimStart('\', '/')
  $assetDestination = Join-Path $webAssets $relativeAsset
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $assetDestination) | Out-Null
  Copy-Item -LiteralPath $assetFile.FullName -Destination $assetDestination
}
foreach ($requiredAsset in @('index.html', 'app.js', 'util\core.js', 'styles\main.css', 'manifest.webmanifest')) {
  if (-not (Test-Path -LiteralPath (Join-Path $webAssets $requiredAsset) -PathType Leaf)) { throw ('缺少离线界面资源：' + $requiredAsset) }
}
Write-Host ('内置离线界面：' + $assetFiles.Count + ' 个静态文件')

Write-Host '[1/6] 编译资源'
& $aapt2 compile --dir (Join-Path $app 'res') -o (Join-Path $work 'res.zip')
if ($LASTEXITCODE -ne 0) { throw 'aapt2 compile 失败' }

Write-Host '[2/6] 链接资源与 manifest'
$linkArgs = @('link', '-o', (Join-Path $work 'base.apk'), '-I', $androidJar, '--manifest', (Join-Path $app 'AndroidManifest.xml'), '-R', (Join-Path $work 'res.zip'), '--java', (Join-Path $work 'gen'), '--min-sdk-version', '24', '--target-sdk-version', '34', '--version-code', '5', '--version-name', '0.5.1', '--auto-add-overlay')
if ($Debuggable) { $linkArgs += '--debug-mode' }
& $aapt2 @linkArgs
if ($LASTEXITCODE -ne 0) { throw 'aapt2 link 失败' }

Write-Host '[3/6] 编译 Java'
$javaFiles = @(Get-ChildItem (Join-Path $app 'java'), (Join-Path $work 'gen') -Recurse -Filter *.java | ForEach-Object { $_.FullName })
& javac -encoding UTF-8 -source 8 -target 8 -bootclasspath $androidJar -classpath $androidJar -d (Join-Path $work 'classes') $javaFiles
if ($LASTEXITCODE -ne 0) { throw 'javac 失败' }

Write-Host '[4/6] 生成 dex'
$classFiles = @(Get-ChildItem (Join-Path $work 'classes') -Recurse -Filter *.class | ForEach-Object { $_.FullName })
& $d8 --lib $androidJar --min-api 24 --output (Join-Path $work 'dex') $classFiles
if ($LASTEXITCODE -ne 0) { throw 'd8 失败' }

Write-Host '[5/6] 打包 / 对齐 / 签名'
Copy-Item (Join-Path $work 'base.apk') (Join-Path $work 'unsigned.apk') -Force
& jar uf (Join-Path $work 'unsigned.apk') -C (Join-Path $work 'dex') classes.dex
if ($LASTEXITCODE -ne 0) { throw '加入 classes.dex 失败' }
# Java jar normalizes ZIP separators. aapt2 -A on Windows can emit backslash
# names (assets/web\index.html), which Android AssetManager cannot resolve.
& jar uf (Join-Path $work 'unsigned.apk') -C $work assets
if ($LASTEXITCODE -ne 0) { throw '加入内置离线界面失败' }
& $zipalign -p -f 4 (Join-Path $work 'unsigned.apk') (Join-Path $work 'aligned.apk')
if ($LASTEXITCODE -ne 0) { throw 'zipalign 失败' }

$keystore = Join-Path $root 'android\keystore\debug.jks'
if (-not (Test-Path $keystore)) {
  New-Item -ItemType Directory -Force -Path (Split-Path $keystore) | Out-Null
  & keytool -genkeypair -keystore $keystore -alias velmira -keyalg RSA -keysize 2048 -validity 10000 -storepass velmira -keypass velmira -dname "CN=Velmira, O=Velmira, C=CN" 2>&1 | Out-Null
  if ($LASTEXITCODE -ne 0) { throw '生成签名密钥失败' }
}

$outPath = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $root $Out }
New-Item -ItemType Directory -Force -Path (Split-Path $outPath) | Out-Null
& $apksigner sign --ks $keystore --ks-key-alias velmira --ks-pass pass:velmira --key-pass pass:velmira --out $outPath (Join-Path $work 'aligned.apk')
if ($LASTEXITCODE -ne 0) { throw 'apksigner sign 失败' }

Write-Host '[6/6] 校验'
$signatureOutput = & $apksigner verify --verbose $outPath
if ($LASTEXITCODE -ne 0) { throw 'APK 签名校验失败' }
$signatureOutput | Select-Object -First 6 | Out-String | Write-Host
$packagedAssets = @(& jar tf $outPath)
if ($LASTEXITCODE -ne 0) { throw '无法检查 APK 内置资源' }
foreach ($requiredAsset in @('assets/web/index.html', 'assets/web/app.js', 'assets/web/util/core.js', 'assets/web/styles/main.css', 'assets/web/manifest.webmanifest')) {
  if ($packagedAssets -notcontains $requiredAsset) { throw ('APK 未包含离线资源：' + $requiredAsset) }
}
Write-Host ('已校验 APK 内置静态资源：' + @($packagedAssets | Where-Object { $_.StartsWith('assets/web/') -and -not $_.EndsWith('/') }).Count + ' 个')
& $aapt2 dump badging $outPath 2>&1 | Select-String -Pattern "^package:|^sdkVersion:|^targetSdkVersion:|^application-label:|^launchable-activity:" | ForEach-Object { Write-Host $_.Line }
$apk = Get-Item $outPath
Write-Host ("APK：" + $apk.FullName + "（" + [math]::Round($apk.Length / 1MB, 2) + " MB）")
exit 0
