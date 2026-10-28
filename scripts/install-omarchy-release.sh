#!/usr/bin/env bash
set -euo pipefail

RELEASE_URL="${PREVIEW_RELEASE_URL:-https://github.com/benclawbot/Preview/releases/latest/download}"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
APP_DIR="$DATA_HOME/Preview"
ICON_DIR="$DATA_HOME/icons/hicolor/512x512/apps"
APPLICATIONS="$DATA_HOME/applications"
APP="$APP_DIR/Preview.AppImage"
ICON="$ICON_DIR/preview.png"
DESKTOP_FILE="$APPLICATIONS/preview.desktop"
MIMEAPPS="$CONFIG_HOME/mimeapps.list"
DESKTOP_ID="preview.desktop"

for command in curl install xdg-mime; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'Required command not found: %s\n' "$command" >&2
    exit 1
  fi
done

tmp="$(mktemp -d)"
trap 'rm -rf -- "$tmp"' EXIT
curl --fail --location --silent --show-error "$RELEASE_URL/Preview-linux-x86_64.AppImage" -o "$tmp/Preview.AppImage"
curl --fail --location --silent --show-error "$RELEASE_URL/Preview-icon.png" -o "$tmp/preview.png"

timestamp="$(date +%Y%m%d-%H%M%S)"
if [[ -f "$DESKTOP_FILE" ]]; then
  cp -p -- "$DESKTOP_FILE" "$DESKTOP_FILE.backup.$timestamp"
fi
if [[ -f "$MIMEAPPS" ]]; then
  cp -p -- "$MIMEAPPS" "$MIMEAPPS.preview-backup.$timestamp"
fi

mkdir -p -- "$APP_DIR" "$ICON_DIR" "$APPLICATIONS"
install -m 755 -- "$tmp/Preview.AppImage" "$APP"
install -m 644 -- "$tmp/preview.png" "$ICON"

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
  application/pdf application/epub+zip image/png image/jpeg image/tiff image/gif
  image/heif image/heic image/webp image/bmp image/svg+xml image/x-canon-cr2
  image/x-nikon-nef image/x-sony-arw image/x-adobe-dng
)
for mime in "${mime_types[@]}"; do
  xdg-mime default "$DESKTOP_ID" "$mime"
done

printf 'Installed Preview to %s\n' "$APP"
printf 'Open Preview from your app launcher or open a supported file in your file manager.\n'
printf 'Previous desktop entry and MIME defaults were backed up with timestamp %s.\n' "$timestamp"
