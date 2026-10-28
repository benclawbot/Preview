#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/src-tauri/target/release/preview"
ICON_SOURCE="$ROOT/src-tauri/icons/preview.svg"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
APPLICATIONS="$DATA_HOME/applications"
ICON_DIR="$DATA_HOME/icons/hicolor/scalable/apps"
DESKTOP_ID="preview.desktop"
DESKTOP_FILE="$APPLICATIONS/$DESKTOP_ID"

if [[ ! -x "$APP" ]]; then
  printf 'Preview executable not found: %s\nBuild it first with: npm run tauri build -- --no-bundle\n' "$APP" >&2
  exit 1
fi
if [[ ! -f "$ICON_SOURCE" ]]; then
  printf 'Preview icon not found: %s\n' "$ICON_SOURCE" >&2
  exit 1
fi
if ! command -v xdg-mime >/dev/null 2>&1; then
  printf 'xdg-mime is required to register file associations.\n' >&2
  exit 1
fi

timestamp="$(date +%Y%m%d-%H%M%S)"
if [[ -f "$DESKTOP_FILE" ]]; then
  cp -p -- "$DESKTOP_FILE" "$DESKTOP_FILE.backup.$timestamp"
fi
MIMEAPPS="$CONFIG_HOME/mimeapps.list"
if [[ -f "$MIMEAPPS" ]]; then
  cp -p -- "$MIMEAPPS" "$MIMEAPPS.preview-backup.$timestamp"
fi

mkdir -p -- "$APPLICATIONS" "$ICON_DIR"
install -m 644 -- "$ICON_SOURCE" "$ICON_DIR/preview.svg"

# Escape desktop-entry metacharacters in the executable path.
APP_EXEC="$APP"
APP_EXEC="${APP_EXEC//\\/\\\\}"
APP_EXEC="${APP_EXEC//\"/\\\"}"
APP_EXEC="${APP_EXEC//%/%%}"
cat >"$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=Preview
Comment=Read EPUB books, view PDFs, and edit images locally
Exec=/usr/bin/env WEBKIT_DISABLE_DMABUF_RENDERER=1 "$APP_EXEC" %f
Icon=preview
Terminal=false
StartupNotify=true
Categories=Graphics;Viewer;
MimeType=application/pdf;application/epub+zip;image/png;image/jpeg;image/tiff;image/gif;image/heif;image/heic;image/webp;image/bmp;image/svg+xml;image/x-canon-cr2;image/x-nikon-nef;image/x-sony-arw;image/x-adobe-dng;
EOF
chmod 644 -- "$DESKTOP_FILE"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APPLICATIONS"
fi

mime_types=(
  application/pdf
  application/epub+zip
  image/png
  image/jpeg
  image/tiff
  image/gif
  image/heif
  image/heic
  image/webp
  image/bmp
  image/svg+xml
  image/x-canon-cr2
  image/x-nikon-nef
  image/x-sony-arw
  image/x-adobe-dng
)

for mime in "${mime_types[@]}"; do
  xdg-mime default "$DESKTOP_ID" "$mime"
done

failed=0
for mime in "${mime_types[@]}"; do
  default="$(xdg-mime query default "$mime" 2>/dev/null || true)"
  if [[ "$default" != "$DESKTOP_ID" ]]; then
    printf 'Could not make Preview the default for %s (current: %s)\n' "$mime" "${default:-none}" >&2
    failed=1
  fi
done
if (( failed )); then
  exit 1
fi

printf 'Installed per-user desktop entry: %s\n' "$DESKTOP_FILE"
printf 'Preview is now the default app for its supported PDF, EPUB, and image types.\n'
printf 'Backed up existing MIME defaults to: %s.preview-backup.%s\n' "$MIMEAPPS" "$timestamp"
