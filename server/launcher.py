"""PyInstaller sidecar entry point; HTTP server only, never opens a window."""
from __future__ import annotations

import argparse
import multiprocessing
import os


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--data-dir", default="")
    args = parser.parse_args()
    if args.data_dir:
        os.environ["DATA_DIR"] = args.data_dir
    import uvicorn
    from server.app.main import app

    uvicorn.run(app, host=args.host, port=args.port, loop="asyncio", http="h11", ws="none", proxy_headers=False)


if __name__ == "__main__":
    multiprocessing.freeze_support()
    main()
