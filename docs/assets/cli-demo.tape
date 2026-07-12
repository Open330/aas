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
Env PS1 "❯ "

Hide
Type "cargo build --release --quiet"
Enter
Type `export PATH="$PWD/target/release:$PATH"`
Enter
Type `rm -rf "$AAS_CONFIG_DIR" && mkdir -p "$AAS_CONFIG_DIR"`
Enter
Type `printf '%s\n' '{"version":1,"accounts":[{"provider":"codex","name":"team","label":"Team","email":"team@example.com","addedAt":"2026-07-12T02:00:00.000Z","profileType":"isolated"},{"provider":"claude","name":"work","label":"Work","email":"work@example.com","addedAt":"2026-07-12T00:00:00.000Z","profileType":"isolated"},{"provider":"codex","name":"personal","label":"Personal","email":"personal@example.com","addedAt":"2026-07-12T01:00:00.000Z","profileType":"isolated"}]}' > "$AAS_CONFIG_DIR/accounts.json"`
Enter
Type `printf '%s\n' '{"claude":"work","codex":"personal","updated":"2026-07-12T03:00:00.000Z"}' > "$AAS_CONFIG_DIR/.active.json"`
Enter
Type "clear"
Enter
Show

Type "aas --version"
Enter
Sleep 2s

Type "aas list"
Enter
Sleep 4s

Type "aas status"
Enter
Sleep 3s

Type "aas usage --help"
Enter
Sleep 5s

Hide
Type `rm -rf "$AAS_CONFIG_DIR"`
Enter
