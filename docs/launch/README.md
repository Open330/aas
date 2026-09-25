# Launch copy

Drafts of public posts about aas (launch posts, release notes, social). Everything here is a
**draft**: the maintainer posts from their own accounts; nothing in this folder is posted
automatically.

Each draft is written in one of two modes, matching the demo video's `--mode`:

| Mode | File | Reader | Include | Leave out |
|---|---|---|---|---|
| intro | [intro.md](intro.md) | Has never seen aas | The problem, 2–3 core scenes, one install line | Version numbers, internal fixes, asx history (one line at most) |
| changes | [changes.md](changes.md) | Already uses or follows aas | User-visible changes only, how to update | The full product pitch (link instead) |

| Channel | Default mode | Demo (`docs/demo/render.sh`) |
|---|---|---|
| README, org site card | intro | `--channel readme` (GIF) |
| Show HN, Reddit, GeekNews | intro | link only |
| X, LinkedIn | intro at launch, then changes | `--channel social`, or `square` for square feeds |
| GitHub release notes | changes | `--channel readme` if useful |

Rules:

- Only describe what the current build does. No "bypass limits" or "unlimited" framing.
- `changes.md` is rewritten for each release from the matching `CHANGELOG.md` section, together
  with `docs/demo/scenes/changes.tape`. Fix the version number and release link before posting.
- Before posting, re-read the channel's rules (Show HN guidelines, subreddit self-promotion
  rules) and re-render the demo for that mode and channel.
