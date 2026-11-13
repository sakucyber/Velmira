"""FastAPI 应用入口：建库 → 挂载业务路由 → 可选挂载扩展路由 → 最后挂静态目录。

项目：Velmira v0.4.0；实时书库 SSE（/api/events）。
启动：python -m uvicorn server.app.main:app --host 127.0.0.1 --port 8765
数据目录：环境变量 DATA_DIR 优先，默认 项目根/data。
"""
from __future__ import annotations

import logging
from contextlib import asynccontextmanager

from fastapi import FastAPI
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from fastapi.staticfiles import StaticFiles

from . import config
from .api.books import router as books_router
from .db import init_db

log = logging.getLogger("velmira.server")


@asynccontextmanager
async def _lifespan(app: FastAPI):
    # 幂等建库：冷启动（无 data/）也能直接服务
    init_db()
    from .manga import queue
    from .ai import book_analysis
    queue.start()
    book_analysis.start()
    try:
        yield
    finally:
        await book_analysis.stop()
        await queue.stop()


def create_app() -> FastAPI:
    app = FastAPI(title=config.APP_NAME, version=config.VERSION, lifespan=_lifespan)

    # 先建库（uvicorn 启动期间也不会出现「表不存在」的窗口）
    init_db()

    @app.exception_handler(RequestValidationError)
    async def _on_validation_error(request, exc: RequestValidationError):
        # 契约 1：错误体统一 {"error": {"code", "message"}}，路径/查询参数非法按 400 返回
        return JSONResponse(status_code=400, content={
            "error": {"code": "invalid_request", "message": "请求参数不合法（路径/查询/请求体类型错误）"}
        })

    @app.get("/api/health")
    def health():
        # 健康检查暴露 Velmira 品牌名与当前版本
        return {
            "ok": True,
            "name": config.APP_NAME,
            "version": config.VERSION,
            "data_dir": str(config.DATA_DIR).replace("\\", "/"),
        }

    app.include_router(books_router)
    from .api.manga import router as manga_router
    from .manga.models import MangaError

    @app.exception_handler(MangaError)
    async def manga_error(request, exc):
        return JSONResponse(status_code=exc.status, content={"error": {"code": exc.code, "message": str(exc)}})

    app.include_router(manga_router)
    from .api.text_ai import router as text_ai_router
    app.include_router(text_ai_router)

    # 契约 8.2：v0.3 实时书库 SSE；模块异常不得拖垮 app
    try:
        from .api.events import router as events_router

        app.include_router(events_router)
    except ImportError:
        pass
    except Exception as exc:
        log.warning("events 路由挂载跳过：%s", exc)

    # 契约 7.4 / 7.5：M1 新增路由；模块异常不得拖垮 app
    try:
        from .api.annotations import router as annotations_router

        app.include_router(annotations_router)
    except ImportError:
        pass
    except Exception as exc:
        log.warning("annotations 路由挂载跳过：%s", exc)

    try:
        from .api.pages import router as pages_router

        app.include_router(pages_router)
    except ImportError:
        pass
    except Exception as exc:
        log.warning("pages/media 路由挂载跳过：%s", exc)

    # 契约第 3 节：C 的 segments / search 路由可选挂载（模块未就绪时不能拖垮 app）
    try:
        from .api.segments import router as segments_router

        app.include_router(segments_router)
    except ImportError:
        pass
    except Exception as exc:  # C 模块存在但暂不可导入时保持服务可用
        log.warning("segments 路由挂载跳过：%s", exc)

    try:
        from .api.search import router as search_router

        app.include_router(search_router)
    except ImportError:
        pass
    except Exception as exc:
        log.warning("search 路由挂载跳过：%s", exc)

    # M2：可配置文本 provider、scene 抽取、人物卡与冻结；模块异常不得拖垮阅读服务。
    try:
        from .api.ai import router as ai_router

        app.include_router(ai_router)
    except ImportError:
        pass
    except Exception as exc:
        log.warning("AI 路由挂载跳过：%s", exc)

    # 静态目录必须最后挂（避免挡住 /api/* 与 /media/*）。web/ 可能尚未就绪，故 check_dir=False。
    if not config.WEB_DIR.is_dir():
        log.info("web/ 目录尚未就绪（%s），静态托管先处于容错状态", config.WEB_DIR)
    app.mount("/", StaticFiles(directory=str(config.WEB_DIR), html=True, check_dir=False))
    return app


app = create_app()


if __name__ == "__main__":  # 便于 python -m server.app.main 直接起开发服务
    import uvicorn

    uvicorn.run("server.app.main:app", host="127.0.0.1", port=8765)
