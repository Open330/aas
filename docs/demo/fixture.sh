# shellcheck shell=bash
# Demo fixture for the README/social recordings. Sourced (hidden) by the generated VHS tape.
# Builds a throwaway aas state with three accounts and stub agent CLIs; touches nothing real.
export AAS_CONFIG_DIR=/tmp/aas-demo/config
export CODEX_HOME=/tmp/aas-demo/native-codex
export CLAUDE_CONFIG_DIR=/tmp/aas-demo/native-claude
rm -rf /tmp/aas-demo
mkdir -p "$AAS_CONFIG_DIR/profiles/codex-personal" "$AAS_CONFIG_DIR/profiles/codex-team" \
  "$AAS_CONFIG_DIR/profiles/claude-work" /tmp/aas-demo/bin "$CODEX_HOME" "$CLAUDE_CONFIG_DIR"

cat > "$AAS_CONFIG_DIR/accounts.json" <<'JSON'
{"version":1,"accounts":[
 {"provider":"claude","name":"work","label":"Work","addedAt":"2026-07-12T00:00:00.000Z","profileType":"isolated"},
 {"provider":"codex","name":"personal","label":"Personal","addedAt":"2026-07-12T01:00:00.000Z","profileType":"isolated"},
 {"provider":"codex","name":"team","label":"Team","addedAt":"2026-07-12T02:00:00.000Z","profileType":"isolated"}]}
JSON
printf '%s\n' '{"claude":"work","codex":"personal","updated":"2026-07-12T03:00:00.000Z"}' > "$AAS_CONFIG_DIR/.active.json"
printf '%s\n' '{"tokens":{"access_token":"demo-personal"}}' > "$AAS_CONFIG_DIR/profiles/codex-personal/auth.json"
printf '%s\n' '{"tokens":{"access_token":"demo-team"}}' > "$AAS_CONFIG_DIR/profiles/codex-team/auth.json"

now=$(($(date +%s) * 1000)); h=$((60 * 60 * 1000))
meter() { printf '{"label":"%s","used_pct":%s,"reset_ms":%s}' "$1" "$2" $((now + $3 * h / 60)); }
entry() { printf '"%s":{"fetchedAtMs":%s,"usage":{"headline":"%s","plan":"%s","meters":[%s,%s],"notes":[],"error":null}}' \
  "$1" "$now" "$2" "$3" "$4" "$5"; }
{
  printf '{"version":1,"entries":{'
  entry claude/work "subscription=max tier=default_claude_max_20x" max \
    "$(meter 5h 86 47)" "$(meter 7d 62 $((52 * 60)))"; printf ','
  entry codex/personal "codex pro" pro "$(meter 5h 12 $((3 * 60 + 12)))" "$(meter 7d 31 $((118 * 60)))"; printf ','
  entry codex/team "codex team" team "$(meter 5h 41 $((1 * 60 + 38)))" "$(meter 7d 57 $((76 * 60)))"
  printf '}}\n'
} > "$AAS_CONFIG_DIR/usage-cache.json"

# Stub agent CLIs: report which profile they were launched with instead of starting a real session.
for agent in codex claude; do
  cat > "/tmp/aas-demo/bin/$agent" <<STUB
#!/bin/sh
home=\${CODEX_HOME:-\${CLAUDE_CONFIG_DIR:-}}
profile=\${home##*/}
case "\$profile" in native-*) profile="your default login" ;; esac
printf '\033[32m✓\033[0m %s session started\n  profile: \033[1m%s\033[0m\n' "$agent" "\$profile"
STUB
  chmod +x "/tmp/aas-demo/bin/$agent"
done

export PATH="/tmp/aas-demo/bin:$PWD/target/release:$PATH"
export PS1='\[\033[38;5;111m\]❯\[\033[0m\] '
# scene "Title" — full-width caption bar shown at the top of a cleared screen.
# end_card "Title" "line"… — closing card with no prompt left on screen.
end_card() { PS1=''; scene "$1"; shift; printf '  %s\n' "$@"; }
scene() { clear; printf '\033[1;38;5;183m%s\033[0m\n\033[38;5;60m%s\033[0m\n\n' "$1" "$(printf '─%.0s' $(seq 1 ${#1}))"; }
