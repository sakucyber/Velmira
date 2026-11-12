"""PyInstaller sidecar entry point; HTTP server only, never opens a window."""
from __future__ import annotations

import argparse
import multiprocessing


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()
    import uvicorn
    from server.app.main import app

    uvicorn.run(app, host=args.host, port=args.port, loop="asyncio", http="h11", ws="none")


if __name__ == "__main__":
    multiprocessing.freeze_support()
    main()
