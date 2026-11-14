# Velmira 运行时发行版说明

本目录是 **Velmira 桌面运行时发行版**，不是源码仓库。可执行文件已经包含运行所需的
Python 后端，目标机器无需另外安装 Python。

## 目录内容

- `Velmira.exe`（Windows）或 `Velmira` / `Velmira.bin`（Linux）：桌面窗口与后端生命周期管理。
- `backend/Velmira-backend/`：Windows 自包含后端（PyInstaller 目录模式）。
- `server/`、`web/`：Linux 包内的源码与前端资源，由启动脚本使用。
- `docs/`：产品、接口、模型池、离线同步和验证文档。
- `README.md`：项目总览。其中提到的 `tests/` 与开发脚本属于源码仓库，不在运行时包内。
- `LICENSE`、`NOTICE.md` 与 `licenses/`：许可与依赖声明清单。

## 版本

- 产品版本：**v0.5.3**
- 健康检查：`GET /api/health` 返回 `{"ok":true,"name":"Velmira","version":"0.5.3"}`
- 本说明与 `START-HERE.txt`、构建脚本使用同一版本号。

## 数据与隐私

- 书库、配置和任务记录保存在用户数据目录：Windows 为
  `%LOCALAPPDATA%\Velmira\data`，Linux 为
  `${XDG_DATA_HOME:-~/.local/share}/Velmira/data`。
- API 密钥只保存在本机数据目录，不会进入书籍快照、公开接口或诊断包。
- 整书分析失败时，可在分析面板点击“诊断”查看阶段与原因，或点击“导出诊断包”
  下载脱敏的任务日志、配置类别与失败样本。

## 获取源码与反馈

源码、测试和开发文档：<https://github.com/sakucyber/Velmira>

遇到 AI 失败时，请附带“导出诊断包”生成的 zip；它不包含 API 密钥或提示词，失败样本只保留有限长度的模型输出片段。
