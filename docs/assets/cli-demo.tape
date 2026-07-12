Require cargo

Output docs/assets/cli-demo.gif

Set Shell "bash"
Set FontSize 16
Set FontFamily "Menlo"
Set LetterSpacing 0
Set LineHeight 1.2
Set Width 920
Set Height 680
Set Padding 24
Set Margin 18
Set MarginFill "#161821"
Set BorderRadius 12
Set WindowBar Colorful
Set Theme "Catppuccin Mocha"
Set TypingSpeed 35ms
Set CursorBlink false
Set Framerate 30

Env AAS_CONFIG_DIR "/tmp/aas-readme-demo"
Env CODEX_HOME "/tmp/aas-readme-native-codex"
Env PS1 "❯ "

Hide
Set TypingSpeed 0ms
Type "cargo build --release --quiet"
Enter
Type `rm -rf "$AAS_CONFIG_DIR" /tmp/aas-readme-bin /tmp/aas-readme-native-codex`
Enter
Type `mkdir -p "$AAS_CONFIG_DIR/profiles/codex-personal" "$AAS_CONFIG_DIR/profiles/codex-team" /tmp/aas-readme-bin /tmp/aas-readme-native-codex`
Enter
Type `printf '%s\n' '{"version":1,"accounts":[{"provider":"codex","name":"team","label":"Team","addedAt":"2026-07-12T02:00:00.000Z","profileType":"isolated"},{"provider":"claude","name":"work","label":"Work","addedAt":"2026-07-12T00:00:00.000Z","profileType":"isolated"},{"provider":"codex","name":"personal","label":"Personal","addedAt":"2026-07-12T01:00:00.000Z","profileType":"isolated"}]}' > "$AAS_CONFIG_DIR/accounts.json"`
Enter
Type `printf '%s\n' '{"claude":"work","codex":"personal","updated":"2026-07-12T03:00:00.000Z"}' > "$AAS_CONFIG_DIR/.active.json"`
Enter
Type `printf '%s\n' '{"tokens":{"access_token":"demo-personal"}}' > "$AAS_CONFIG_DIR/profiles/codex-personal/auth.json"`
Enter
Type `printf '%s\n' '{"tokens":{"access_token":"demo-team"}}' > "$AAS_CONFIG_DIR/profiles/codex-team/auth.json"`
Enter
Type `now_ms=$(($(date +%s) * 1000)); reset_5h=$((now_ms + 3 * 60 * 60 * 1000)); reset_7d=$((now_ms + 5 * 24 * 60 * 60 * 1000)); printf '{"version":1,"entries":{"claude/work":{"fetchedAtMs":%s,"usage":{"headline":"subscription=max tier=default_claude_max_20x","plan":"max","meters":[{"label":"5h","used_pct":18,"reset_ms":%s},{"label":"7d","used_pct":42,"reset_ms":%s}],"notes":[],"error":null}},"codex/personal":{"fetchedAtMs":%s,"usage":{"headline":"codex pro","plan":"pro","meters":[{"label":"5h","used_pct":8,"reset_ms":%s},{"label":"7d","used_pct":31,"reset_ms":%s}],"notes":[],"error":null}},"codex/team":{"fetchedAtMs":%s,"usage":{"headline":"codex team","plan":"team","meters":[{"label":"5h","used_pct":27,"reset_ms":%s},{"label":"7d","used_pct":54,"reset_ms":%s}],"notes":[],"error":null}}}}\n' "$now_ms" "$reset_5h" "$reset_7d" "$now_ms" "$reset_5h" "$reset_7d" "$now_ms" "$reset_5h" "$reset_7d" > "$AAS_CONFIG_DIR/usage-cache.json"`
Enter
Type `printf '%s\n' '#!/bin/sh' 'profile=${CODEX_HOME##*/}' 'account=${profile#codex-}' 'printf "✓ isolated Codex session\n  account: %s\n  profile: %s\n" "$account" "$profile"' > /tmp/aas-readme-bin/codex && chmod +x /tmp/aas-readme-bin/codex`
Enter
Type `export PATH="/tmp/aas-readme-bin:$PWD/target/release:$PATH"`
Enter
Type "clear"
Enter
Set TypingSpeed 35ms
Show

Type "aas usage"
Enter
Sleep 5s

Type "aas e personal"
Enter
Sleep 3s

Type "aas e team"
Enter
Sleep 5s

Hide
Type `rm -rf "$AAS_CONFIG_DIR" /tmp/aas-readme-bin /tmp/aas-readme-native-codex`
Enter
