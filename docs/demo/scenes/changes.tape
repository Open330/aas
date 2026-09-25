# changes: what is new since the last release (release notes, update posts).
# Rewrite these scenes for each release from CHANGELOG.md, alongside docs/launch/changes.md.

Hide
Type `scene "New: a provider name runs its active account"`
Enter
Show
Type "aas active codex"
Sleep 300ms
Enter
Sleep 1.2s
Type "aas exec codex"
Sleep 300ms
Enter
Sleep 2.5s

Hide
Type `scene "Switch, and the provider name follows"`
Enter
Show
Type "aas switch codex team"
Sleep 300ms
Enter
Sleep 1s
Type "aas exec codex"
Sleep 300ms
Enter
Sleep 3s

Hide
Type `end_card "Update: brew upgrade aas" "Release notes: github.com/Open330/aas/releases"`
Enter
Show
Sleep 3s
