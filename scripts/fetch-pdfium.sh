#!/usr/bin/env bash
set -euo pipefail

# Keep this revision synchronized with the native library tested by Preview.
readonly PDFIUM_TAG="chromium/8125"
readonly RELEASE_TAG="chromium%2F8125"
readonly ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly DEST="$ROOT/src-tauri/vendor/pdfium/lib"

os="$(uname -s)"
arch="$(uname -m)"
case "$os:$arch" in
  Linux:x86_64|Linux:amd64)
    asset="pdfium-linux-x64.tgz"
    library="libpdfium.so"
    source="lib/libpdfium.so"
    ;;
  Linux:aarch64|Linux:arm64)
    asset="pdfium-linux-arm64.tgz"
    library="libpdfium.so"
    source="lib/libpdfium.so"
    ;;
  Darwin:x86_64|Darwin:arm64)
    asset="pdfium-mac-univ.tgz"
    library="libpdfium.dylib"
    source="lib/libpdfium.dylib"
    ;;
  MINGW*:x86_64|MSYS*:x86_64|CYGWIN*:x86_64)
    asset="pdfium-win-x64.tgz"
    library="pdfium.dll"
    source="bin/pdfium.dll"
    ;;
  MINGW*:aarch64|MSYS*:aarch64|CYGWIN*:aarch64)
    asset="pdfium-win-arm64.tgz"
    library="pdfium.dll"
    source="bin/pdfium.dll"
    ;;
  *)
    printf 'Unsupported host: %s (%s)\n' "$os" "$arch" >&2
    exit 2
    ;;
esac

url="https://github.com/bblanchon/pdfium-binaries/releases/download/${RELEASE_TAG}/${asset}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

printf 'Fetching PDFium %s (%s)…\n' "$PDFIUM_TAG" "$asset"
curl --fail --location --silent --show-error "$url" -o "$tmp/pdfium.tgz"
tar -xzf "$tmp/pdfium.tgz" -C "$tmp"
if [[ ! -f "$tmp/$source" ]]; then
  printf 'Expected library not found in %s: %s\n' "$asset" "$source" >&2
  tar -tzf "$tmp/pdfium.tgz" | head -n 30 >&2
  exit 1
fi

mkdir -p "$DEST"
install -m 755 "$tmp/$source" "$DEST/$library"
if [[ -f "$tmp/LICENSE" ]]; then
  install -m 644 "$tmp/LICENSE" "$ROOT/src-tauri/vendor/pdfium/LICENSE"
fi
if [[ -d "$tmp/licenses" ]]; then
  mkdir -p "$ROOT/src-tauri/vendor/pdfium/licenses"
  cp -a "$tmp/licenses/." "$ROOT/src-tauri/vendor/pdfium/licenses/"
fi
printf 'Installed %s\n' "$DEST/$library"
