#!/usr/bin/env bash
# Render the aas demo with VHS.
#
#   docs/demo/render.sh [--mode intro|changes] [--channel readme|social|square] [--out PATH]
#
#   --mode intro      first-time introduction (README, launch posts)             [default]
#   --mode changes    what is new since the last release (release notes, update posts)
#   --channel readme  1000px-wide GIF for README.md                              [default]
#   --channel social  1280x720 MP4 for X, LinkedIn, YouTube
#   --channel square  860x860 MP4 for feeds that crop to square (scaled up by the feed)
#
# Scenes live in docs/demo/scenes/<mode>.tape. docs/demo/fixture.sh builds a throwaway state
# under /tmp/aas-demo, so no real account is touched. Requires vhs and cargo. If vhs stops at
# "could not open ttyd: EOF", run with VHS_NO_SANDBOX=true.
set -euo pipefail

mode=intro channel=readme out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --mode) mode="$2"; shift 2 ;;
    --channel) channel="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

root="$(cd "$(dirname "$0")/../.." && pwd)"
scenes="docs/demo/scenes/$mode.tape"
[ -f "$root/$scenes" ] || { echo "no scenes for mode '$mode' ($scenes)" >&2; exit 2; }

case "$channel" in
  readme) width=1000 height=560 font=16 pad=28 margin=24 ext=gif ;;
  social) width=1280 height=720 font=20 pad=28 margin=24 ext=mp4 ;;
  # On-screen text size depends on how many columns the canvas holds, not on its pixels: the
  # usage table needs 80, so this is sized to 81 (measured with `tput cols` under VHS) and the
  # feed scales it up.
  square) width=860 height=860 font=15 pad=8 margin=0 ext=mp4 ;;
  *) echo "unknown channel: $channel" >&2; exit 2 ;;
esac

# The intro GIF is the README asset; everything else is a build output for posting.
if [ -z "$out" ]; then
  if [ "$mode-$channel" = "intro-readme" ]; then
    out="docs/assets/cli-demo.gif"
  else
    out="target/demo/aas-$mode-$channel.$ext"
  fi
fi

cd "$root"
mkdir -p "$(dirname "$out")"
cargo build --release --quiet

tmp="$(mktemp -d "${TMPDIR:-/tmp}/aas-demo.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
tape="$tmp/demo.tape"
{
  cat <<TAPE
Output "$out"
Set Shell "bash"
Set FontFamily "Menlo"
Set FontSize $font
Set LineHeight 1.25
Set Width $width
Set Height $height
Set Padding $pad
Set Margin $margin
Set MarginFill "#1e1e2e"
Set BorderRadius 14
Set WindowBar Colorful
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms
Set CursorBlink false
Set Framerate 30

Hide
Type "source docs/demo/fixture.sh"
Enter
Type "clear"
Enter
Show
TAPE
  cat "$scenes"
  cat <<'TAPE'

Hide
Type "rm -rf /tmp/aas-demo"
Enter
TAPE
} > "$tape"

vhs "$tape"
echo "wrote $out"
