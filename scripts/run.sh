#!/usr/bin/env bash
# Velmira source server for Linux/macOS. Credentials stay in DATA_DIR.
set -euo pipefail

bind_address="127.0.0.1"
server_port="8765"
data_directory=""
reload_arguments=()
while (($#)); do
  case "$1" in
    --host|--port|--data-dir)
      if (($# < 2)) || [[ "$2" == --* ]]; then
        printf 'Missing value for %s\n' "$1" >&2
        exit 2
      fi
      case "$1" in
        --host) bind_address="$2" ;;
        --port) server_port="$2" ;;
        --data-dir) data_directory="$2" ;;
      esac
      shift 2 ;;
    --reload) reload_arguments=(--reload); shift ;;
    --help|-h)
      printf '%s\n' 'Usage: bash scripts/run.sh [--host 127.0.0.1] [--port 8765] [--data-dir PATH] [--reload]'
      exit 0 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done
if [[ ! "$server_port" =~ ^[0-9]+$ ]] || ((server_port < 1 || server_port > 65535)); then
  printf '%s\n' 'Port must be between 1 and 65535.' >&2
  exit 2
fi
project_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$project_directory"
if [[ -n "$data_directory" ]]; then export DATA_DIR="$data_directory"; fi
if [[ -n "${VELMIRA_PYTHON:-}" ]]; then
  python_executable="$VELMIRA_PYTHON"
elif command -v python3 >/dev/null 2>&1; then
  python_executable="python3"
else
  python_executable="python"
fi
printf 'Velmira: http://%s:%s/\n' "$bind_address" "$server_port"
exec "$python_executable" -m uvicorn server.app.main:app --host "$bind_address" --port "$server_port" --no-proxy-headers "${reload_arguments[@]}"
