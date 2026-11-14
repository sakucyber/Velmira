# v0.5.1 手机 AI 与系统栏验证

验证日期：2026-11-14；设备为 `127.0.0.1:16384` 的 Android 15 / WebView 110 虚拟机。

| 本轮检查 | 结果 |
| --- | --- |
| 系统栏跟随主题 | 读取真实 MainActivity Window 状态，日间/羊皮纸/夜间颜色及系统图标明暗均通过；夜间重启后保持 |
| 手机 AI 本地集成 | 真实 APK Java 代理与本地模拟模型服务联调：503 后切换同连接备用模型、轮询、提示词/记忆注入、凭据不回显通过 |
| 并发阅读 | AI 请求进行时，书库 API 仍能响应 |
| 手机生图与搜索 | 模拟服务返回的 base64 原图保存/解码和 SearXNG 来源链接通过 |
| 设置与工作台 | 390px 模型/搜索/记忆/提示词无横向溢出；真实触摸拖动排序保存并恢复、阅读器 AI 带入当前作品、文本与生图 UI、历史/记忆跨 force-stop 持久化通过 |
| 真实文本模型 | 手机直接调用 `gpt-6-luna` 成功，回答覆盖给定原文事件，无降级 |
| 真实生图模型 | 手机直接调用 `agnes-image-2.5-flash`，生成并保存可解码的 1024×1024 图片，无降级；已与桌面适配器一致请求 `b64_json` |
| 阅读回归 | TXT/EPUB/CBZ 导入与阅读、插图、进度/书签/批注、恶意导入拒绝、重启持久化通过 |
| 其他界面回归 | 个性化引导与七项手机设置入口、桌面八项设置回归通过，零 JS 异常 |
| APK 产物 | 0.5.1 / versionCode 5，非调试包安装与启动通过，设备 flags 无 DEBUGGABLE；Java/d8/zipalign 与 v2/v3 签名通过；35 个内置 Web 文件与源码逐字节一致，未发现嵌入 API 密钥 |

手机真实云端验收覆盖上述两种主模型；其他备用模型的当次云端可用性未逐一验证。故障切换与负载分配使用本地模拟服务验证。联网搜索本轮使用本地 SearXNG 模拟服务；使用真实搜索供应商前仍需在设置中配置搜索服务。

APK 内无 API 密钥。验收虚拟机的密钥仅从既有本机私有配置转入应用私有存储；新设备安装后需在设置中填写自己的密钥并启用模型池。阅读可离线使用，云端 AI 和联网搜索需网络；整本自动漫画任务链仍由电脑服务执行。

```text
node tests/cdp_onboarding.mjs
node tests/run_cdp_ai_settings.mjs
node tests/android/cdp_standalone.mjs
node tests/android/cdp_mobile_ai.mjs
node tests/android/cdp_mobile_ui.mjs
# 可选：调用真实云端模型并将已有桌面凭据配置到指定模拟器
node tests/android/live_mobile_ai.mjs --use-desktop-credentials
```

`cdp_mobile_ai.mjs` 会在设备已有密钥时跳过，避免替换无法从脱敏快照恢复的凭据。

本轮安装包：`dist/Velmira-mobile-ai-v0.5.1.apk`；通用副本为
`dist/Velmira-universal.apk`，校验文件为 `dist/SHA256SUMS-v0.5.1.txt`。

## v0.5.0 离线与同步验证记录

验证日期：2026-11-14。桌面/浏览器测试使用独立临时目录；Android 测试仅导入
带「验收」标记的生成文件，不调用真实云端模型。

| 检查 | 结果 |
| --- | --- |
| Python 完整测试 | 223 passed，另 38 subtests passed |
| 本机存储快速回归 | 凭据隔离、605 条分批推送、冲突选择、设备迁移、未来时间戳通过 |
| 真实 IndexedDB 事务 | 故障注入后创建/修改/删除回滚，冲突替换与迁移回滚，双标签序号通过 |
| 文字离线闭环 | 断网读取、搜索、书签/批注编辑、Service Worker 重载、回网自动同步通过 |
| 两独立客户端 | 同时修改批注保留冲突，显式选择后另一设备拉取收敛 |
| CBZ 与 EPUB | 完整下载，禁 HTTP 缓存后断网重载、漫画翻页及插图 Blob 解码通过 |
| 下载清理 | 删除本机正文、图片和下载标记，服务端原书保留 |
| 设置界面 | 8 项入口、实际下载与同步、冲突卡、清除确认、设备管理在 390px 无溢出，零 JS 异常 |
| Windows 自包含包 | 构建成功；WebView 227ms，书架就绪 881ms；退出清理通过 |
| 打包后端 | 0.4.2 健康检查、导入、设备注册、push/pull/snapshot 通过 |
| Android APK | Java/d8/zipalign 成功，v2/v3 签名通过，versionCode 4，33 个内置静态文件 |
| Android 独立本机书库（PJJ110，Android 15，WebView 110，`127.0.0.1:16384`） | release APK 安装成功；首次打开无需地址；个性化引导可跳过且持久化；真实 WebView 导入 TXT、EPUB（含内嵌图片）、CBZ；正文/漫画阅读、搜索、进度、书签、批注、移出/恢复通过；force-stop 后书库与记录保留；重复启动维持单一 loopback 书库；恶意 ZIP 路径、XXE XML 拒绝；纯图片 EPUB 进度保存通过 |
| 产物一致性 | Windows 全部 Web 文件、APK 全部静态资源与最终源码逐字节一致 |
| 静态检查 | 全部 Web JS 语法、git diff --check、Bash 启动脚本语法与帮助通过 |

Python 运行仅有依赖的 Starlette/httpx 弃用提示；Android 编译仅有 Java 8
与旧 API 弃用提示。最终 WebView 验收脚本全部通过。独立 app_process 导入探针
完成全部断言后在虚拟机运行时退出阶段出现 SIGSEGV；实际 APK 内导入与重启测试
未出现该故障，因此不把该探针计为完整通过的进程测试。

## 平台边界

本次在 Android 15 虚拟机 PJJ110 上以 debug APK 完成 WebView 真实导入与 force-stop 重启验收，随后安装同源码的非调试 APK 并检查启动、版本及本地健康接口；尚未在 Android 真机上做同等系统级测试。模拟器使用本机端口 `127.0.0.1:16384`，应用内书库默认 loopback 端口 `18765`。
Linux、macOS 和 iOS 浏览器没有新增系统实测；Linux/macOS 复用同一 Python
服务与浏览器前端，iOS 没有原生 IPA。普通浏览器离线启动需要 HTTPS 或
localhost，Android APK 内置界面可用于局域网 HTTP。

当前同步范围为进度、书签、批注；逐设备下载 TXT/EPUB/CBZ。长期记忆、
模型设置和 AI 任务由服务本机管理。云端 AI 与联网搜索仍需服务可达。

## 复现

```text
python -m pytest -q
node tests/test_local_store.mjs
node tests/cdp_offline.mjs
node tests/cdp_offline_media.mjs
node tests/run_cdp_ai_settings.mjs
node tests/android/cdp_standalone.mjs
node scripts/measure-startup.mjs --exe dist/Velmira-desktop-v0.4.2/Velmira.exe
```

Android CDP 脚本需要先安装 `-Debuggable` 构建并启动应用，默认连接
`127.0.0.1:16384`；可用 `ANDROID_SERIAL` / `ANDROID_ADB` 覆盖设备与 adb 路径。

旧版本安装包：`dist/Velmira-standalone-v0.5.0.apk`。旧版本桌面包与验证记录仍保留在 `dist/`；`Velmira-universal.apk` 随最新安装包更新。
