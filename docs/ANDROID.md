# Velmira Android（通用 APK）

Velmira 的 Android 客户端：用 WebView 连接**局域网里的 Velmira 桌面服务**，手机上继续读同一书库。系统要求 Android 7.0+（minSdk 24），一个 APK 覆盖所有 ABI（纯 Java + WebView，无原生库）。

## 为什么是「连接桌面服务」而不是离线版

Velmira 的后端是 Python（FastAPI + SQLite），桌面端也依赖它。Android 客户端复用同一套 Web 界面与 API，把它当作移动端遥控/阅读器：

- 书库、阅读、书签、注释、实时书库、离线进度都在桌面端数据库里，手机与电脑看到同一份数据；
- 手机与电脑需要在同一 Wi-Fi；桌面端启动服务后，手机填电脑的局域网 IP 与端口（默认 8765）；
- 完全离线、手机本地存书的版本需要把后端移植到 Android（或嵌入 Python 运行时），属于后续工作，不在本 APK 范围。

## 构建

~~~powershell
# 需要 Android SDK（cmdline-tools + build-tools 35 + platform android-34）
# 项目脚本可用 .toolchains/android/sdk，或设置 ANDROID_HOME
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-android.ps1
# 产物：dist/Velmira-universal.apk
~~~

脚本不依赖 Gradle：aapt2 编译资源 → aapt2 link → javac → d8 → zipalign → apksigner（v2/v3 签名）。首次构建会在 `android/keystore/debug.jks` 生成自签名密钥（口令 velmira），仅用于侧载测试；正式发布请换成自己的 keystore。

## 安装与使用

1. 电脑端启动 Velmira：`powershell -File scripts/run.ps1`（或桌面端 Velmira.exe）；
2. 查电脑局域网 IP：`ipconfig` 里的 IPv4 地址，确认 8765 端口可访问（Windows 防火墙放行 Python/端口）；
3. 手机安装 APK（允许「安装未知来源应用」），打开 Velmira，输入 `http://<电脑IP>:8765`，点「连接」；
4. 连接成功后进入书架。返回键：阅读页 → 回书架；书架 → 服务器设置；设置页 → 退出。

支持的手机端交互：书架/阅读/书签/注释/搜索/目录/漫画翻页，主题与字号；导入文件走系统文件选择器（TXT/EPUB/CBZ），上传到桌面服务。

## 已知限制

- 依赖桌面端服务在线；手机与电脑必须同一网络（或可路由的局域网/VPN）。
- 首次连接使用明文 HTTP（局域网），未做 TLS；不要在公网暴露 8765。
- 未在真机/模拟器上跑过的状态（构建环境没有加速模拟器）：UI 与 API 已与桌面 Web 版共用，Android 壳只负责连接、WebView 与文件选择器。
- 桌面端服务默认绑定 127.0.0.1：要在手机上访问，需要监听局域网地址：
  - Web 版：`powershell -File scripts/run.ps1 -BindAddress 0.0.0.0`
  - 桌面端：`desktop\Velmira.exe -bind 0.0.0.0`
  - Windows 防火墙需放行 8765 端口。
