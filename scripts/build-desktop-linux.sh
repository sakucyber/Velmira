#!/usr/bin/env bash
# Build a Linux native desktop bundle (MyGo + source Python backend).
# WebKitGTK is provided by the target distribution on 64-bit desktops. The
# 32-bit launcher uses the system browser. Python dependencies are installed
# into the bundle's private venv on first launch.
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
version="${VELMIRA_VERSION:-0.5.1}"
arch="${VELMIRA_ARCH:-$(uname -m)}"
case "$arch" in
  x86_64|amd64) arch="amd64" ;;
  aarch64|arm64) arch="arm64" ;;
  armv7l|armhf|arm) arch="arm" ;;
  i386|i686|386) arch="386" ;;
  *) echo "Unsupported Linux architecture: $arch" >&2; exit 2 ;;
esac
go_bin="${VELMIRA_GO:-$(command -v go || true)}"
if [[ -z "$go_bin" ]]; then
  echo "Go 1.27+ is required (set VELMIRA_GO to its executable)." >&2
  exit 2
fi

stage="$root/dist/Velmira-desktop-v${version}-linux-${arch}"
archive="$root/dist/Velmira-desktop-v${version}-linux-${arch}.tar.gz"
rm -rf "$stage" "$archive"
mkdir -p "$stage"

printf 'Building Linux desktop shell (%s)...\n' "$arch"
if [[ "$arch" == "arm" ]]; then
  (cd "$root/desktop" && GOOS=linux GOARCH=arm GOARM=7 CGO_ENABLED=0 "$go_bin" build -tags portable32 -trimpath -ldflags '-s -w' -o "$stage/Velmira.bin" .)
elif [[ "$arch" == "386" ]]; then
  (cd "$root/desktop" && GOOS=linux GOARCH=386 CGO_ENABLED=0 "$go_bin" build -tags portable32 -trimpath -ldflags '-s -w' -o "$stage/Velmira.bin" .)
else
  (cd "$root/desktop" && GOOS=linux GOARCH="$arch" CGO_ENABLED=0 "$go_bin" build -trimpath -ldflags '-s -w' -o "$stage/Velmira.bin" .)
fi
cp -a "$root/server" "$stage/server"
cp -a "$root/web" "$stage/web"
cp "$root/server/requirements.txt" "$stage/requirements.txt"
if [[ "$arch" == "arm" || "$arch" == "386" ]]; then
  # The local memory counter has an explicitly labelled estimate fallback.
  # Avoid forcing unsupported 32-bit Rust extension builds for this optional
  # accelerator during first launch.
  sed -i '/^tiktoken/d' "$stage/requirements.txt"
elif [[ -d "$root/.tmp/tiktoken-cache" ]]; then
  cp -a "$root/.tmp/tiktoken-cache" "$stage/tiktoken-cache"
fi
cp "$root/LICENSE" "$stage/LICENSE" 2>/dev/null || true
cp "$root/NOTICE.md" "$stage/NOTICE.md" 2>/dev/null || true

cat > "$stage/Velmira" <<'LAUNCHER'
#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
python_bin="${VELMIRA_PYTHON:-$root/.venv/bin/python}"
if [[ ! -x "$python_bin" ]]; then
  system_python="${PYTHON3:-$(command -v python3 || command -v python || true)}"
  [[ -n "$system_python" ]] || { echo 'Python 3 is required.' >&2; exit 1; }
  "$system_python" -m venv "$root/.venv"
  python_bin="$root/.venv/bin/python"
fi
if ! "$python_bin" -c 'import fastapi, uvicorn, PIL, qrcode' >/dev/null 2>&1; then
  "$python_bin" -m pip install -r "$root/requirements.txt"
fi
export VELMIRA_PYTHON="$python_bin"
export DATA_DIR="${DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/Velmira/data}"
exec "$root/Velmira.bin" -root "$root" "$@"
LAUNCHER
chmod +x "$stage/Velmira" "$stage/Velmira.bin"

cat > "$stage/install.sh" <<'INSTALLER'
#!/usr/bin/env bash
set -euo pipefail
source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
install_dir="${VELMIRA_INSTALL_DIR:-$HOME/.local/opt/velmira}"
mkdir -p "$install_dir" "$HOME/.local/share/applications"
cp -a "$source_dir/." "$install_dir/"
cat > "$HOME/.local/share/applications/velmira.desktop" <<DESKTOP
[Desktop Entry]
Name=Velmira
Comment=Local-first novel reader
Exec=$install_dir/Velmira
Terminal=false
Type=Application
Categories=Office;Literature;
DESKTOP
printf 'Installed Velmira to %s\n' "$install_dir"
printf 'Start it from the application menu or run %s/Velmira\n' "$install_dir"
INSTALLER
chmod +x "$stage/install.sh"
if [[ "$arch" == "arm" || "$arch" == "386" ]]; then
  sed -i 's/Terminal=false/Terminal=true/' "$stage/install.sh"
fi

tar -C "$root/dist" -czf "$archive" "$(basename "$stage")"
printf 'Bundle: %s\nArchive: %s\n' "$stage" "$archive"
