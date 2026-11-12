# 构建 Velmira Android 通用 APK（不依赖 Gradle）
# 用法：powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-android.ps1
param(
  [string]$Sdk = "",
  [string]$Out = "dist\Velmira-universal.apk"
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
if (Test-Path $work) { Remove-Item $work -Recurse -Force }
New-Item -ItemType Directory -Force -Path (Join-Path $work 'gen'), (Join-Path $work 'classes'), (Join-Path $work 'dex') | Out-Null

Write-Host '[1/6] 编译资源'
& $aapt2 compile --dir (Join-Path $app 'res') -o (Join-Path $work 'res.zip')
if ($LASTEXITCODE -ne 0) { throw 'aapt2 compile 失败' }

Write-Host '[2/6] 链接资源与 manifest'
& $aapt2 link -o (Join-Path $work 'base.apk') -I $androidJar --manifest (Join-Path $app 'AndroidManifest.xml') -R (Join-Path $work 'res.zip') --java (Join-Path $work 'gen') --min-sdk-version 24 --target-sdk-version 34 --version-code 1 --version-name 0.4.0 --auto-add-overlay
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
& $apksigner verify --verbose $outPath | Select-Object -First 6 | Out-String | Write-Host
& $aapt2 dump badging $outPath 2>&1 | Select-String -Pattern "^package:|^sdkVersion:|^targetSdkVersion:|^application-label:|^launchable-activity:" | ForEach-Object { Write-Host $_.Line }
$apk = Get-Item $outPath
Write-Host ("APK：" + $apk.FullName + "（" + [math]::Round($apk.Length / 1MB, 2) + " MB）")
exit 0
