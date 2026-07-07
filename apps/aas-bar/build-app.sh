#!/bin/sh
# Build aas-bar as a proper macOS .app bundle.
#
# A SwiftUI MenuBarExtra only shows its menubar item when run from a bundled .app with an
# Info.plist (LSUIElement) — not as a bare `swift run` executable. This assembles that bundle.
#
#   ./build-app.sh            # -> ./AasBar.app  (run with: open ./AasBar.app)
#   ./build-app.sh --install  # also copies it to /Applications
set -e
cd "$(dirname "$0")"

echo "building release binary…"
swift build -c release

APP="AasBar.app"
BINDIR="$(swift build -c release --show-bin-path)"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp Info.plist "$APP/Contents/Info.plist"
cp "$BINDIR/AasBar" "$APP/Contents/MacOS/AasBar"
# Copy the SPM resource bundle (brand logos) so Bundle.module resolves inside the .app.
for b in "$BINDIR"/*.bundle; do
    [ -d "$b" ] || continue
    cp -R "$b" "$APP/Contents/Resources/"
    cp -R "$b" "$APP/Contents/MacOS/"
done
# Ad-hoc sign so macOS is happy launching a locally-built app.
codesign --force --sign - "$APP" 2>/dev/null || true

echo "built $APP"

if [ "$1" = "--install" ]; then
    rm -rf "/Applications/$APP"
    cp -R "$APP" "/Applications/$APP"
    echo "installed /Applications/$APP"
fi
