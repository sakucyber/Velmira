# Velmira Desktop（MyGo）

原生桌面壳：用 [MyGo](https://github.com/egoist/mygo)（Go 桌面框架，MIT）把 Velmira 的 Web 界面装进系统 WebView2 窗口，并由它启动/关闭本地 Python 后端。

## 为什么是「壳」而不是重写

Velmira 的界面是零依赖原生 ESM，后端是 Python（FastAPI + SQLite）。桌面端把两者装进一个窗口：

- 启动时自动在空闲端口拉起内置后端（源码开发也可用 `python -m uvicorn server.app.main:app`），等 `/api/health` 就绪后把窗口导航过去；
- 退出时杀掉后端进程树，不留孤儿服务；
- 已有开发服务（8765）在跑时直接复用，方便调试；
- 窗口 1280×860（最小 900×600），原生菜单（文件/视图/窗口），系统标题栏。

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

## 开发

~~~powershell
cd desktop
go mod tidy
go vet ./...
go build -o Velmira.exe .
~~~

## 已知限制

- 源码运行仍可使用本机 Python；分发目录由 `build-desktop-bundle.ps1` 生成 `backend/Velmira-backend.exe`，目标机器无需安装 Python。分发包数据存放在 `%LOCALAPPDATA%\Velmira\data`；源码工作区继续使用项目 `data/`，`DATA_DIR` 可覆盖两者。
- 目前只测 Windows（WebView2）；MyGo 支持 macOS/Linux，进程管理在非 Windows 上退化为普通 `Kill`。
- 窗口只有一个；多窗口、托盘、自动更新等桌面能力留给后续。

## 许可

桌面壳使用 MyGo（MIT License，版权归 MyGo contributors），见仓库根目录 [NOTICE.md](../NOTICE.md)。
