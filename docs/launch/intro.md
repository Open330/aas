# Launch copy: intro

> **Draft. Not posted.** First-time introduction of aas, for readers who have never seen it.
> Demo: `docs/demo/render.sh --mode intro --channel <readme|social|square>`.
> See [README](README.md) for the rules these drafts follow.

## Show HN

**Title**

> Show HN: aas – use several Claude Code and Codex accounts side by side

**Body**

> I use a personal and a work account for Claude Code and Codex, and logging out and back in to move between them got old. aas is a single static binary (Rust) that keeps each account in its own profile and lets you:
>
> - run one session under another account (`aas exec work`) while your default login stays as it was
> - switch the default in one command (`aas switch codex team`)
> - see every account's remaining usage in one table (`aas usage`, fetched in parallel and cached)
>
> Credentials live in per-account 0600 files or the OS keychain; nothing is sent anywhere except the providers' own endpoints. It supports Claude Code, Codex, Grok/xAI, Z.AI, Kimi/Moonshot, Cursor and Pi, and runs on macOS, Linux and Windows.
>
> Install: `brew install open330/tap/aas` (or a curl/PowerShell installer that verifies the release attestation).
>
> It's MIT-licensed. I'd love feedback on the exec/switch model and on providers you'd want next.
>
> https://github.com/Open330/aas

**Prepared reply: provider terms**

> aas only manages accounts you already have and sign in to yourself; it doesn't share, pool or resell access. Whether a given plan allows more than one account is up to each provider's terms, so please check those for your own setup.

## Reddit (r/ClaudeAI, r/ChatGPTCoding)

**Title**

> I made a small CLI to use multiple Claude Code / Codex accounts without logging out

**Body**

> If you keep separate personal and work accounts, aas lets you run a session on one account while your default login stays on the other, switch the default in one command, and see all accounts' remaining usage in one table. Single Rust binary, credentials stay in local 0600 files or your keychain.
>
> `brew install open330/tap/aas` · https://github.com/Open330/aas
>
> Happy to answer questions. Feedback on what's confusing is especially welcome.

(Re-read each subreddit's self-promotion rules right before posting.)

## X (attach `--channel social`)

> Use several Claude Code and Codex accounts side by side.
>
> aas runs one session on another account without touching your default login, switches in one command, and shows every account's remaining usage at a glance.
>
> brew install open330/tap/aas
> github.com/Open330/aas

## LinkedIn (attach `--channel square`)

> I built aas, a small open-source CLI for people who use more than one coding-agent account (Claude Code, Codex and others).
>
> Instead of logging out and back in, you can run a single session on another account while your default stays put, switch the default in one command, and check every account's remaining usage in one table.
>
> It's a single Rust binary, MIT-licensed, and installs with `brew install open330/tap/aas`.
> https://github.com/Open330/aas

## GeekNews (Korean)

**Title**

> aas - 여러 Claude Code·Codex 계정을 로그아웃 없이 나란히 쓰는 CLI

**Body**

> 개인 계정과 회사 계정을 오갈 때마다 로그아웃하고 다시 로그인하는 게 번거로워서 만든 도구입니다. Rust로 만든 단일 바이너리입니다.
>
> - `aas exec work`: 다른 계정으로 세션 하나만 실행하고, 기본 로그인은 그대로 둡니다.
> - `aas switch codex team`: 기본 계정을 명령 한 줄로 바꿉니다.
> - `aas usage`: 모든 계정의 남은 사용량을 표 하나로 보여 줍니다(병렬 조회, 캐시).
>
> 자격 증명은 계정별 0600 파일이나 OS 키체인에만 저장합니다. Claude Code, Codex, Grok/xAI, Z.AI, Kimi/Moonshot, Cursor, Pi를 지원하고 macOS, Linux, Windows에서 동작합니다.
>
> 설치: `brew install open330/tap/aas`
> https://github.com/Open330/aas
