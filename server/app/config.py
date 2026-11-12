"""全局配置：项目根、数据目录、静态目录、上传上限。

项目：Velmira，版本 0.4.0。
数据目录优先级：环境变量 DATA_DIR > 自包含版用户目录或源码项目根/data。
注意：DATA_DIR 在进程导入时读取，测试需在启动/导入前设置环境变量。
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

VERSION = "0.4.0"

# 产品名（契约 7.1）：UI / 健康检查统一使用
APP_NAME = "Velmira"

# server/app/config.py -> parents[2] = 项目根
PROJECT_ROOT = Path(__file__).resolve().parents[2]

_env_data_dir = os.environ.get("DATA_DIR", "").strip()
if _env_data_dir:
    DATA_DIR = Path(_env_data_dir).expanduser()
elif getattr(sys, "frozen", False):
    # 分发包可以放在只读目录；用户书库独立于升级/解压路径。
    DATA_DIR = Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "Velmira" / "data"
else:
    DATA_DIR = PROJECT_ROOT / "data"
DATA_DIR = DATA_DIR.resolve()

# 前端静态目录（B 维护；可能尚不存在，main.py 挂载时容错）
WEB_DIR = PROJECT_ROOT / "web"

# 上传上限：64MB（契约 4.2）
MAX_UPLOAD_BYTES = 64 * 1024 * 1024
