#!/bin/sh
# Build aas-bar as a proper macOS .app bundle.
#
# A SwiftUI MenuBarExtra only shows its menubar item when run from a bundled .app with an
# Info.plist (LSUIElement) — not as a bare `swift run` executable. This assembles that bundle.
#
#   ./build-app.sh            # -> ./AasBar.app  (run with: open ./AasBar.app)
#   ./build-app.sh --install  # also copies it to /Applications
set -eu
cd "$(dirname "$0")"

echo "building release binary…"
swift build -c release

APP="AasBar.app"
BINDIR="$(swift build -c release --show-bin-path)"
STAGE=".${APP}.stage.$$"
BACKUP=".${APP}.backup.$$"
INSTALL_STAGE=""
INSTALL_BACKUP=""

cleanup() {
    rm -rf "$STAGE"
    if [ -e "$BACKUP" ]; then
        if [ ! -e "$APP" ]; then
            mv "$BACKUP" "$APP"
        else
            rm -rf "$BACKUP"
        fi
    fi
    if [ -n "$INSTALL_STAGE" ]; then
        rm -rf "$INSTALL_STAGE"
    fi
    if [ -n "$INSTALL_BACKUP" ] && [ -e "$INSTALL_BACKUP" ]; then
        if [ ! -e "/Applications/$APP" ]; then
            mv "$INSTALL_BACKUP" "/Applications/$APP"
        else
            rm -rf "$INSTALL_BACKUP"
        fi
    fi
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$STAGE/Contents/MacOS" "$STAGE/Contents/Resources"
cp Info.plist "$STAGE/Contents/Info.plist"
cp "$BINDIR/AasBar" "$STAGE/Contents/MacOS/AasBar"
# Copy resource files into the app's standard resource directory. The app checks Bundle.main
# first and only falls back to SwiftPM's Bundle.module when run directly from `.build`.
for b in "$BINDIR"/*.bundle; do
    [ -d "$b" ] || continue
    find "$b" -type f -name '*.png' -exec cp {} "$STAGE/Contents/Resources/" \;
done
# Ad-hoc sign and verify strictly; never print success for an invalid bundle.
codesign --force --deep --sign - "$STAGE"
codesign --verify --deep --strict --verbose=2 "$STAGE"

# Keep the prior known-good bundle until the staged replacement is fully assembled and verified.
if [ -e "$APP" ]; then
    mv "$APP" "$BACKUP"
fi
if ! mv "$STAGE" "$APP"; then
    [ ! -e "$BACKUP" ] || mv "$BACKUP" "$APP"
    exit 1
fi
rm -rf "$BACKUP"

echo "built $APP"

if [ "${1:-}" = "--install" ]; then
    INSTALL_STAGE="/Applications/.${APP}.stage.$$"
    INSTALL_BACKUP="/Applications/.${APP}.backup.$$"
    rm -rf "$INSTALL_STAGE" "$INSTALL_BACKUP"
    cp -R "$APP" "$INSTALL_STAGE"
    codesign --verify --deep --strict --verbose=2 "$INSTALL_STAGE"
    if [ -e "/Applications/$APP" ]; then
        mv "/Applications/$APP" "$INSTALL_BACKUP"
    fi
    if ! mv "$INSTALL_STAGE" "/Applications/$APP"; then
        [ ! -e "$INSTALL_BACKUP" ] || mv "$INSTALL_BACKUP" "/Applications/$APP"
        exit 1
    fi
    rm -rf "$INSTALL_BACKUP"
    echo "installed /Applications/$APP"
fi
