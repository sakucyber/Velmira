# Velmira Desktop（MyGo）

原生桌面壳：用 [MyGo](https://github.com/egoist/mygo)（Go 桌面框架，MIT）把 Velmira 的 Web 界面装进系统 WebView2 窗口，并由它启动/关闭本地 Python 后端。

## 为什么是「壳」而不是重写

Velmira 的界面是零依赖原生 ESM，后端是 Python（FastAPI + SQLite）。桌面端把两者装进一个窗口：

- 启动时并行初始化 WebView2 和内置后端，窗口先显示“正在打开书架”，`/api/health` 就绪后在同一窗口进入书架；启动中关闭也会结束自己启动的后端。源码开发也可用 `python -m uvicorn server.app.main:app`；
- 退出时杀掉后端进程树，不留孤儿服务；
- 已有开发服务（8765）在跑时直接复用，方便调试；
- 窗口 1280×860（最小 900×600），系统标题栏；桌面壳不显示原生菜单。
- 禁用 WebView 默认右键菜单；书架的侧栏、搜索框、封面和空白处都可接收拖入的 TXT / EPUB / CBZ 文件，松开即导入。
- 书架支持筛选、排序与标准/紧凑密度；卡片 `⋯` 可查看详情、重命名、移出或恢复。移出保留全部阅读与 AI 数据，可从“状态 → 已移出”找回。
- 阅读页右上角“设置”集中放置主题、字号和快捷键说明；底部只保留阅读进度。漫画设置提供主题与适页/适宽。

真正的功能（导入、阅读、注释、实时书库）仍在 Web 前端与 Python 后端里，桌面层只负责窗口、生命周期与系统集成。

## 环境

| 依赖 | 版本 | 说明 |
| --- | --- | --- |
| Go | ≥ 1.27 | MyGo 要求；仓库脚本可用 `.toolchains/go` |
| Python | 3.11+（含 fastapi/uvicorn/Pillow） | 源码运行或构建需要；自包含分发包运行时无需安装 |
| WebView2 Runtime | Windows 10+ | Windows 11 自带 |

不需要 Bun / Node：直接用 `go build`，不经过 MyGo CLI 的 Vite 脚手架。

## 构建与运行

~~~powershell
# 一键构建（自动寻找 PATH 或 .toolchains/go/bin/go.exe）
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-desktop.ps1

# 构建无需安装 Python 的自包含分发目录（需要构建机已安装 PyInstaller）
python -m pip install -r server/requirements.txt pyinstaller
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-desktop-bundle.ps1

# 运行
desktop\Velmira.exe

# 截图模式（启动、等页面、截窗口、退出；用于验收/CI）
desktop\Velmira.exe -screenshot docs\screenshots\desktop-app.png -wait 6s
~~~

Linux 原生桌面包在 Linux 主机上构建：

~~~bash
bash scripts/build-desktop-linux.sh
# 输出 dist/Velmira-desktop-v0.5.2-linux-amd64.tar.gz 或 arm64 包
~~~

Linux 运行时需要 GTK 3、WebKitGTK 4.1、Python 3 和 `python3-venv`。
包内 `Velmira` 启动器会在第一次运行时创建私有虚拟环境并安装后端依赖，
之后可以离线启动。`install.sh` 会创建用户级桌面菜单项。

命令行参数：

| 参数 | 作用 |
| --- | --- |
| `-screenshot <path>` | 页面加载后截取窗口 PNG 并退出 |
| `-wait <duration>` | 截图前额外等待（默认 5s） |
| `-port <n>` | 后端端口（默认 `VELMIRA_PORT` 或 8765；占用时自动换空闲端口） |
| `-bind <address>` | 后端监听地址（默认 127.0.0.1；手机局域网连接可用 0.0.0.0） |
| `-root <dir>` | 后端根目录（默认优先从可执行文件目录寻找内置后端或源码） |
| `-python <exe>` | 显式指定 Python 可覆盖内置后端；也支持 `VELMIRA_PYTHON` |

环境变量：`VELMIRA_ROOT`、`VELMIRA_PORT`、`VELMIRA_PYTHON`、`DATA_DIR`（传给后端）。

后端日志：`%LOCALAPPDATA%\Velmira\backend.log`。

隔离启动测试（使用临时书库和 WebView 配置，不读取实际书库）：

~~~powershell
node scripts/measure-startup.mjs --exe desktop/Velmira.exe
node scripts/measure-startup.mjs --scenario early-close
node scripts/measure-startup.mjs --scenario failure
~~~

输出进程启动至 WebView 可连接、书架可操作的毫秒数。首次磁盘读取、杀毒扫描和已有书库大小都会影响结果；比较时使用相同机器、后端和测试条件。异常场景使用临时假后端验证报错和退出清理。

## 开发

~~~powershell
cd desktop
go mod tidy
go vet ./...
go build -trimpath -ldflags "-H=windowsgui -s -w" -o Velmira.exe .
~~~

## 已知限制

- 源码运行仍可使用本机 Python；分发目录由 `build-desktop-bundle.ps1` 生成目录模式的 `backend/Velmira-backend/`，目标机器无需安装 Python，也避免每次启动解压单文件后端。分发包数据存放在 `%LOCALAPPDATA%\Velmira\data`；源码工作区继续使用项目 `data/`，`DATA_DIR` 可覆盖两者。
- Windows 使用 WebView2，Linux 使用 WebKitGTK；Linux 包需要目标发行版提供 GTK/WebKitGTK 动态库。
  当前自动化窗口验收仍以 Windows 为主，Linux 原生包已完成交叉编译和资源/启动器校验，需在目标 Linux 桌面环境做最终显示验收。
- 窗口只有一个；多窗口、托盘、自动更新等桌面能力留给后续。

## 许可

桌面壳使用 MyGo（MIT License，版权归 MyGo contributors），见仓库根目录 [NOTICE.md](../NOTICE.md)。
