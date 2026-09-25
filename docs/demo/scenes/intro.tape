# intro: first-time introduction of aas (README, launch posts, landing pages).

Hide
Type `scene "Several agent accounts on one machine"`
Enter
Show
Type "aas list"
Sleep 300ms
Enter
Sleep 2.5s

Hide
Type `scene "Every account's remaining usage at a glance"`
Enter
Show
Type "aas usage"
Sleep 300ms
Enter
Sleep 4s

Hide
Type `scene "One session on another account, default login untouched"`
Enter
Show
Type "aas exec team"
Sleep 300ms
Enter
Sleep 1.5s
Type "codex"
Sleep 300ms
Enter
Sleep 2.5s

Hide
Type `scene "Or switch the default in one command"`
Enter
Show
Type "aas switch codex team"
Sleep 300ms
Enter
Sleep 2.5s

Hide
Type `end_card "aas: Agent Account Switcher" "brew install open330/tap/aas" "github.com/Open330/aas"`
Enter
Show
Sleep 3.5s
