# Requires a JDK, Android SDK/build-tools 35, NDK r27, and the requested Rust Android targets.
# With no -Abi argument, builds the three per-ABI APKs and a universal APK.
param(
    [string]$Sdk,
    [string]$Ndk,
    [string]$BuildToolsVersion = '35.0.0',
    [ValidateRange(35, 999)] [int]$TargetApi = 35,
    [ValidateSet('arm64-v8a', 'armeabi-v7a', 'x86_64', 'universal')] [string[]]$Abi,
    [string]$OutDirectory = 'dist',
    [string]$TargetDirectory = $env:CARGO_TARGET_DIR,
    [string]$Keystore = $env:VELMIRA_ANDROID_KEYSTORE,
    [string]$KeyAlias = 'velmira',
    [ValidateRange(0, 2100000000)] [int]$VersionCode = 0,
    [switch]$Debuggable
)

$ErrorActionPreference = 'Stop'
$repository = Split-Path -Parent $PSScriptRoot
$appDirectory = Join-Path $repository 'apps/android'
$minApi = 24
$ndkVersion = '27.2.12479018'
$isWindowsHost = [IO.Path]::DirectorySeparatorChar -eq '\'
$executableSuffix = if ($isWindowsHost) { '.exe' } else { '' }
$scriptSuffix = if ($isWindowsHost) { '.bat' } else { '' }
$nativeTargets = [ordered]@{
    'arm64-v8a'   = @('aarch64-linux-android', 'aarch64-linux-android')
    'armeabi-v7a' = @('armv7-linux-androideabi', 'armv7a-linux-androideabi')
    'x86_64'      = @('x86_64-linux-android', 'x86_64-linux-android')
}

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) {
        return [IO.Path]::GetFullPath($Path)
    }
    return [IO.Path]::GetFullPath((Join-Path $repository $Path))
}

function Find-JdkTool([string]$Name) {
    if ($env:JAVA_HOME) {
        return Join-Path $env:JAVA_HOME "bin/$Name$executableSuffix"
    }
    return (Get-Command "$Name$executableSuffix" -CommandType Application | Select-Object -First 1).Source
}

function Invoke-BuildTool([string]$Tool, [string[]]$ToolArguments) {
    & $Tool @ToolArguments
    if ($LASTEXITCODE -ne 0) {
        throw "$(Split-Path -Leaf $Tool) failed with exit code $LASTEXITCODE."
    }
}

function Build-NativeLibrary([string]$NativeAbi) {
    $rustTarget, $clangTriple = $nativeTargets[$NativeAbi]
    $clangTarget = "$clangTriple$minApi"
    $compilerSuffix = if ($isWindowsHost) { '.cmd' } else { '' }
    $targetVariable = $rustTarget.Replace('-', '_')
    $compilerVariables = @{
        "CARGO_TARGET_$($targetVariable.ToUpperInvariant())_LINKER" = $clang
        "CC_$rustTarget" = Join-Path $ndkBin "$clangTarget-clang$compilerSuffix"
        "CXX_$rustTarget" = Join-Path $ndkBin "$clangTarget-clang++$compilerSuffix"
        "AR_$rustTarget" = Join-Path $ndkBin "llvm-ar$executableSuffix"
        "CXXSTDLIB_$rustTarget" = 'c++_static'
    }
    $oldVariables = @{}
    $cargoArguments = @(
        'rustc', '--locked', '--package', 'velmira-android', '--lib',
        '--target', $rustTarget, '--target-dir', $TargetDirectory
    )
    if (!$Debuggable) { $cargoArguments += '--release' }
    $cargoArguments += @('--', "-Clink-arg=--target=$clangTarget", '-Clink-arg=-Wl,-z,max-page-size=16384')

    try {
        foreach ($name in $compilerVariables.Keys) {
            $oldVariables[$name] = [Environment]::GetEnvironmentVariable($name)
            [Environment]::SetEnvironmentVariable($name, $compilerVariables[$name])
        }
        Write-Host "Compiling $NativeAbi ($rustTarget)"
        Invoke-BuildTool $cargo $cargoArguments
    } finally {
        foreach ($name in $oldVariables.Keys) {
            [Environment]::SetEnvironmentVariable($name, $oldVariables[$name])
        }
    }

    $nativeLibrary = Join-Path $TargetDirectory "$rustTarget/$rustProfile/libvelmira_android.so"
    $segments = @(Invoke-BuildTool $readelf @('-lW', $nativeLibrary) | Select-String '^\s*LOAD\s+.*\s(0x[0-9a-f]+)\s*$')
    if (!$segments -or @($segments | Where-Object { [Convert]::ToInt32($_.Matches[0].Groups[1].Value, 16) -lt 16384 }).Count) {
        throw "ELF load segments are missing or are not aligned for 16 KiB pages: $nativeLibrary"
    }
    $symbols = @(Invoke-BuildTool $readelf @('--dyn-syms', '--wide', $nativeLibrary))
    if (!($symbols -match '\bGLOBAL\s+DEFAULT\s+\d+\s+ANativeActivity_onCreate\s*$')) {
        throw "NativeActivity entry point is not exported: $nativeLibrary"
    }

    $packageLibraryDirectory = Join-Path $buildWork "libraries/lib/$NativeAbi"
    New-Item -ItemType Directory -Path $packageLibraryDirectory -Force | Out-Null
    Copy-Item -LiteralPath $nativeLibrary -Destination $packageLibraryDirectory
}

if (!$Sdk) { $Sdk = $env:ANDROID_HOME }
if (!$Sdk) { $Sdk = $env:ANDROID_SDK_ROOT }
if (!$Sdk) { $Sdk = '.toolchains/android/sdk' }
$Sdk = Resolve-RepositoryPath $Sdk
if (!$Ndk) { $Ndk = $env:ANDROID_NDK_HOME }
if (!$Ndk) { $Ndk = Join-Path $Sdk "ndk/$ndkVersion" }
$Ndk = Resolve-RepositoryPath $Ndk
if (!(Test-Path -LiteralPath (Join-Path $Ndk 'source.properties') -PathType Leaf)) {
    throw "Install Android NDK r27 with sdkmanager 'ndk;$ndkVersion' or pass -Ndk."
}
if (!(Select-String -LiteralPath (Join-Path $Ndk 'source.properties') -Pattern '^Pkg.Revision\s*=\s*27\.')) {
    throw "Expected Android NDK r27 at $Ndk."
}

$ndkHost = if ($isWindowsHost) { 'windows-x86_64' } elseif ($IsMacOS) { 'darwin-x86_64' } else { 'linux-x86_64' }
$ndkBin = Join-Path $Ndk "toolchains/llvm/prebuilt/$ndkHost/bin"
$clang = Join-Path $ndkBin "clang$executableSuffix"
$readelf = Join-Path $ndkBin "llvm-readelf$executableSuffix"
$buildTools = Join-Path $Sdk "build-tools/$BuildToolsVersion"
$androidJar = Join-Path $Sdk "platforms/android-$TargetApi/android.jar"
$aapt2 = Join-Path $buildTools "aapt2$executableSuffix"
$d8 = Join-Path $buildTools "d8$scriptSuffix"
$zipalign = Join-Path $buildTools "zipalign$executableSuffix"
$apksigner = Join-Path $buildTools "apksigner$scriptSuffix"
$javac = Find-JdkTool 'javac'
$jar = Find-JdkTool 'jar'
$keytool = Find-JdkTool 'keytool'
$cargo = (Get-Command cargo -CommandType Application | Select-Object -First 1).Source
foreach ($requiredPath in @($clang, $readelf, $androidJar, $aapt2, $d8, $zipalign, $apksigner, $javac, $jar, $keytool)) {
    if (!(Test-Path -LiteralPath $requiredPath -PathType Leaf)) { throw "Required tool or SDK component is missing: $requiredPath" }
}

if (!$TargetDirectory) { $TargetDirectory = 'target' }
$TargetDirectory = Resolve-RepositoryPath $TargetDirectory
$OutDirectory = Resolve-RepositoryPath $OutDirectory
$rustProfile = if ($Debuggable) { 'debug' } else { 'release' }
$packageNames = @($nativeTargets.Keys) + 'universal'
if ($Abi) { $packageNames = @($packageNames | Where-Object { $_ -in $Abi }) }
$buildAbis = @($nativeTargets.Keys | Where-Object { $_ -in $packageNames -or 'universal' -in $packageNames })
$manifest = Join-Path $appDirectory 'AndroidManifest.xml'
$javaSources = @(Get-ChildItem -LiteralPath (Join-Path $appDirectory 'java') -File -Filter '*.java' -Recurse | ForEach-Object FullName)
if (!$javaSources) { throw "Android bridge source is missing under $appDirectory/java." }

if ($Keystore) {
    $Keystore = Resolve-RepositoryPath $Keystore
    if (!(Test-Path -LiteralPath $Keystore -PathType Leaf)) { throw "Keystore is missing: $Keystore" }
    if (!$env:VELMIRA_ANDROID_STORE_PASSWORD -or !$env:VELMIRA_ANDROID_KEY_PASSWORD) {
        throw 'Set VELMIRA_ANDROID_STORE_PASSWORD and VELMIRA_ANDROID_KEY_PASSWORD for the supplied keystore.'
    }
    $storePass = 'env:VELMIRA_ANDROID_STORE_PASSWORD'
    $keyPass = 'env:VELMIRA_ANDROID_KEY_PASSWORD'
} else {
    $Keystore = Resolve-RepositoryPath '.toolchains/android/velmira-native-development.p12'
    $KeyAlias = 'velmira-native-development'
    $developmentPassword = 'velmira-native-dev'
    $storePass = "pass:$developmentPassword"
    $keyPass = $storePass
    New-Item -ItemType Directory -Path (Split-Path -Parent $Keystore) -Force | Out-Null
    Write-Host "Using the local Velmira Native development key at $Keystore"
    if (!(Test-Path -LiteralPath $Keystore -PathType Leaf)) {
        $newKeystore = "$Keystore.$([Guid]::NewGuid().ToString('N')).tmp"
        try {
            Invoke-BuildTool $keytool @(
                '-genkeypair', '-storetype', 'PKCS12', '-keystore', $newKeystore, '-alias', $KeyAlias,
                '-keyalg', 'RSA', '-keysize', '2048', '-validity', '10000',
                '-storepass', $developmentPassword, '-keypass', $developmentPassword,
                '-dname', 'CN=Velmira Native Development', '-noprompt'
            )
            try { [IO.File]::Move($newKeystore, $Keystore) }
            catch [IO.IOException] { if (!(Test-Path -LiteralPath $Keystore -PathType Leaf)) { throw } }
        } finally {
            if (Test-Path -LiteralPath $newKeystore -PathType Leaf) { Remove-Item -LiteralPath $newKeystore }
        }
    }
}

$buildWork = Join-Path $TargetDirectory "native-android-apk/$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $buildWork, $OutDirectory -Force | Out-Null
Push-Location -LiteralPath $repository
try {
    $metadata = (Invoke-BuildTool $cargo @('metadata', '--locked', '--format-version', '1', '--no-deps')) | ConvertFrom-Json
    $package = $metadata.packages | Where-Object Name -eq 'velmira-android' | Select-Object -First 1
    if (!$package) { throw 'The Cargo workspace does not contain velmira-android.' }
    $version = $package.version
    if (!$VersionCode) {
        $major, $minor, $patch = (($version -split '[-+]', 2)[0] -split '\.') | ForEach-Object { [long]$_ }
        $derivedVersionCode = $major * 1000000 + $minor * 1000 + $patch
        if ($minor -ge 1000 -or $patch -ge 1000 -or $derivedVersionCode -lt 1 -or $derivedVersionCode -gt 2100000000) {
            throw "Cannot derive an Android version code from $version. Pass -VersionCode."
        }
        $VersionCode = $derivedVersionCode
    }

    foreach ($nativeAbi in $buildAbis) { Build-NativeLibrary $nativeAbi }

    $classes = Join-Path $buildWork 'classes'
    $dex = Join-Path $buildWork 'dex'
    $baseApk = Join-Path $buildWork 'base.apk'
    $compiledResources = Join-Path $buildWork 'resources.zip'
    New-Item -ItemType Directory -Path $classes, $dex -Force | Out-Null
    Invoke-BuildTool $aapt2 @('compile', '--dir', (Join-Path $appDirectory 'res'), '-o', $compiledResources)
    $linkArguments = @(
        'link', '-o', $baseApk, '-I', $androidJar, '--manifest', $manifest,
        '-R', $compiledResources, '--auto-add-overlay',
        '--min-sdk-version', $minApi, '--target-sdk-version', $TargetApi, '--version-code', $VersionCode, '--version-name', $version
    )
    if ($Debuggable) { $linkArguments += '--debug-mode' }
    Invoke-BuildTool $aapt2 $linkArguments
    Invoke-BuildTool $javac (@('-encoding', 'UTF-8', '-source', '8', '-target', '8', '-bootclasspath', $androidJar, '-d', $classes) + $javaSources)
    $classFiles = @(Get-ChildItem -LiteralPath $classes -File -Filter '*.class' -Recurse | ForEach-Object FullName)
    Invoke-BuildTool $d8 (@('--lib', $androidJar, '--min-api', $minApi, '--output', $dex) + $classFiles)
    Invoke-BuildTool $jar @('u0f', $baseApk, '-C', $dex, 'classes.dex')

    foreach ($packageName in $packageNames) {
        $fileName = "Velmira-android-$packageName.apk"
        $unsigned = Join-Path $buildWork "unsigned-$fileName"
        $aligned = Join-Path $buildWork "aligned-$fileName"
        $signed = Join-Path $buildWork $fileName
        $packageAbis = if ($packageName -eq 'universal') { @($nativeTargets.Keys) } else { @($packageName) }
        Copy-Item -LiteralPath $baseApk -Destination $unsigned

        $jarArguments = @('u0f', $unsigned)
        foreach ($packageAbi in $packageAbis) {
            $jarArguments += @('-C', (Join-Path $buildWork 'libraries'), "lib/$packageAbi/libvelmira_android.so")
        }
        Invoke-BuildTool $jar $jarArguments
        Invoke-BuildTool $zipalign @('-P', '16', '-f', '4', $unsigned, $aligned)
        Invoke-BuildTool $apksigner @(
            'sign', '--ks', $Keystore, '--ks-key-alias', $KeyAlias, '--ks-pass', $storePass,
            '--key-pass', $keyPass, '--out', $signed, $aligned
        )
        Invoke-BuildTool $apksigner @('verify', '--verbose', $signed) | Out-Null
        Invoke-BuildTool $zipalign @('-c', '-P', '16', '4', $signed)

        $badging = @(Invoke-BuildTool $aapt2 @('dump', 'badging', $signed))
        if (!($badging -match "^package: name='dev\.velmira\.nativeapp'") -or
            !($badging -match "^launchable-activity: name='dev\.velmira\.nativeapp\.MainActivity'")) {
            throw "APK package or launcher identity is incorrect: $fileName"
        }
        $entries = @(Invoke-BuildTool $jar @('tf', $signed))
        $expectedLibraries = @($packageAbis | ForEach-Object { "lib/$_/libvelmira_android.so" })
        $actualLibraries = @($entries | Where-Object { $_ -match '^lib/.+\.so$' })
        if ($entries -notcontains 'classes.dex' -or $expectedLibraries.Count -ne $actualLibraries.Count -or
            (Compare-Object $expectedLibraries $actualLibraries)) {
            throw "Unexpected native libraries or missing classes.dex: $fileName"
        }
    }

    foreach ($packageName in $packageNames) {
        $fileName = "Velmira-android-$packageName.apk"
        $outputApk = Join-Path $OutDirectory $fileName
        Move-Item -LiteralPath (Join-Path $buildWork $fileName) -Destination $outputApk -Force
        Write-Host "Verified APK: $outputApk"
    }
} finally {
    Pop-Location
    $resolvedWork = (Resolve-Path -LiteralPath $buildWork).ProviderPath
    if ($resolvedWork -eq $buildWork) { Remove-Item -LiteralPath $resolvedWork -Recurse -Force }
}
