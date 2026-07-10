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
# Copy resource files into the app's standard resource directory. The app checks Bundle.main
# first and only falls back to SwiftPM's Bundle.module when run directly from `.build`.
for b in "$BINDIR"/*.bundle; do
    [ -d "$b" ] || continue
    find "$b" -type f -name '*.png' -exec cp {} "$APP/Contents/Resources/" \;
done
# Ad-hoc sign and verify strictly; never print success for an invalid bundle.
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

echo "built $APP"

if [ "$1" = "--install" ]; then
    rm -rf "/Applications/$APP"
    cp -R "$APP" "/Applications/$APP"
    echo "installed /Applications/$APP"
fi
