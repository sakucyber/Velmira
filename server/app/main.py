"""FastAPI 应用入口：建库 → 挂载业务路由 → 可选挂载扩展路由 → 最后挂静态目录。

项目：Velmira v0.5.3；离线同步与实时书库 SSE。
启动：python -m uvicorn server.app.main:app --host 127.0.0.1 --port 8765
数据目录：环境变量 DATA_DIR 优先，默认 项目根/data。
"""
from __future__ import annotations

import logging
import time
from contextlib import asynccontextmanager

from fastapi import FastAPI
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from fastapi.staticfiles import StaticFiles

from . import config
from .api.books import router as books_router
from .db import init_db

log = logging.getLogger("velmira.server")
# Desktop redirects stdout/stderr to backend.log. Without a root handler the
# application loggers only emit WARNING+ through lastResort, which loses the
# context needed to diagnose an AI failure; keep INFO+ with timestamps.
logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s")


@asynccontextmanager
async def _lifespan(app: FastAPI):
    # create_app() 已完成建库；这里避免启动阶段再次读 schema、做迁移事务。
    from .manga import queue, automatic, single_page
    from .ai import book_analysis
    log.info("Velmira %s starting", config.VERSION)
    single_page.recover()
    queue.start()
    book_analysis.start()
    automatic.start()
    try:
        yield
    finally:
        await automatic.stop()
        await book_analysis.stop()
        await queue.stop()
        log.info("Velmira backend stopped")


def create_app() -> FastAPI:
    app = FastAPI(title=config.APP_NAME, version=config.VERSION, lifespan=_lifespan)
    from .security import install_security
    install_security(app)

    # 先建库（uvicorn 启动期间也不会出现「表不存在」的窗口）
    init_db()

    @app.middleware("http")
    async def _request_timing(request, call_next):
        # Only paths and durations are logged; query strings and bodies can
        # contain book text or search terms.
        started = time.monotonic()
        try:
            response = await call_next(request)
        except Exception:
            log.exception("API 请求异常 %s %s", request.method, request.url.path)
            raise
        elapsed = time.monotonic() - started
        if elapsed >= 2.0 and request.url.path.startswith("/api/"):
            log.warning("API 请求较慢 %s %s %.2fs status=%s", request.method,
                        request.url.path, elapsed, response.status_code)
        return response

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
        }

    # 阻断旧地址，避免升级目录残留的开发笔记继续公开。
    @app.get("/MEMORY.md", include_in_schema=False)
    def hidden_memory():
        return JSONResponse(status_code=404, content={"detail": "Not Found"})

    app.include_router(books_router)
    from .api.manga import router as manga_router
    from .manga.models import MangaError

    @app.exception_handler(MangaError)
    async def manga_error(request, exc):
        return JSONResponse(status_code=exc.status, content={"error": {"code": exc.code, "message": str(exc)}})

    app.include_router(manga_router)
    from .api.manga_pages import router as manga_pages_router
    app.include_router(manga_pages_router)
    from .api.auto_manga import router as auto_manga_router
    app.include_router(auto_manga_router)
    from .api.single_page import router as single_page_router
    app.include_router(single_page_router)
    from .api.text_ai import router as text_ai_router
    app.include_router(text_ai_router)
    from .api.storyboards import router as storyboards_router
    app.include_router(storyboards_router)
    from .api.ai_features import router as ai_features_router
    from .api.catalog import router as catalog_router
    from .api.configuration import router as configuration_router
    app.include_router(ai_features_router)
    app.include_router(catalog_router)
    app.include_router(configuration_router)
    from .api.security import router as security_router
    app.include_router(security_router)
    from .api.sync import router as sync_router
    app.include_router(sync_router)

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

    uvicorn.run("server.app.main:app", host="127.0.0.1", port=8765, proxy_headers=False)
