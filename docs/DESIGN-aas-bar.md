# aas-bar — macOS menubar companion — Design Document

> Status: **v1 built** (read-only), **SwiftUI**. A menubar glance for `aas`. Companion to
> [`DESIGN.md`](DESIGN.md); reads data from the `aas` CLI via `aas usage --json`.

`aas-bar` puts every LLM account's remaining quota in the macOS menubar as a **ring gauge**.
Click it to open a native popover — a card per account with health-colored usage bars and
reset times. It exists so you can answer *"am I about to run out?"* without opening a terminal
and waiting on `aas usage`.

- **App:** `apps/aas-bar` — a SwiftUI `MenuBarExtra` app (Swift Package, `swift build`).
- **Platform:** macOS 14+ (menubar). Other platforms are out of scope.
- **Engine:** the `aas` CLI stays the data engine; the app shells out to `aas usage --json`
  and renders the result. `aas` is located via `$AAS_BIN`, then `~/.cargo/bin`, then `PATH`.

> **Why SwiftUI (history).** v1 was first built in Rust — a `muda` text menu, then an
> egui/`winit`/`glutin` popover with a hand-rasterized ring and `NSVisualEffectView`
> vibrancy. That stack was fragile (the menubar icon silently failed to appear) and couldn't
> be visually verified. `MenuBarExtra` is the purpose-built API: the icon always shows, the
> popover gets native material for free, SF Pro + light/dark + the ring `Gauge` are built in.
> The Rust `crates/aas-bar` was retired; its `snapshot()` fetch became `aas usage --json`.

---

## 1. Goals & Non-Goals

### Goals
1. **Glance in the menubar.** One colored ring summarizes the *worst* remaining quota across
   all accounts. No terminal required.
2. **Details on click.** A dropdown lists each account grouped by provider, with a linear
   meter, used-%, and reset time — the same data `aas usage` shows.
3. **Reuse the core.** Shell out to the installed `aas usage --json`; provider logic and
   credentials remain in Rust and the Swift app parses a stable JSON contract.
4. **Native and dependency-light.** A SwiftUI app bundle with no Electron/webview runtime.
5. **Cheap to run.** Show a persistent cache and fetch only on first empty launch or explicit
   Refresh, with CLI backoff plus an app-side cooldown.

### Non-Goals (v1)
- **No mutations.** Read-only. No `switch`, no `login`, no `exec`. (v2.)
- **No notifications / threshold alerts.** (v2.)
- **Non-macOS.** Windows/Linux tray is possible with the same crates later, not now.

---

## 2. Confirmed decisions

| Decision | Choice | Rationale |
|---|---|---|
| Stack | SwiftUI `MenuBarExtra` app (`apps/aas-bar`) | Purpose-built API; native, reliable, verifiable |
| Data source | shell `aas usage --json` | Reuses the CLI engine; clean, versioned contract |
| v1 scope | Read-only glance | Fastest path to "check without a terminal" |
| Glance surface | Ring gauge (arc = worst usage, health-colored) | Encodes level + urgency, not a flat dot |
| Detail surface | `MenuBarExtra(.window)` popover, cards | Native material, SF Pro, light/dark — all free |
| Look | System-native (SF Pro, materials, system colors) | "Melts into macOS" without fighting a GL stack |

---

## 3. Architecture

```
apps/aas-bar (SwiftUI, Swift Package → executable "AasBar")
  ├─ AasBarApp        @main App: MenuBarExtra { PopoverView } label: { MenuBarLabel }
  │                   .menuBarExtraStyle(.window)   // native material popover
  │   └─ AppDelegate  setActivationPolicy(.accessory)   // no Dock icon
  ├─ MenuBarLabel     RingGauge(fraction, color) — Circle().trim(...) health-colored ring
  ├─ PopoverView      header (status chip + updated) · ScrollView of provider groups
  │   ├─ AccountCard  ● active · name · plan chip · error/headline/meters
  │   └─ MeterRow     label · Capsule gauge (fill = used%, health color) · % · time-left
  └─ UsageModel       @MainActor ObservableObject
        ├─ cache + explicit refresh() → runs `aas usage --json` off the main thread
        ├─ 90s subprocess deadline; terminate/kill fallback; bounded diagnostics
        └─ JSONDecoder → [Account]; @Published drives the label + popover
                    │
                    ▼  shells out to
  aas usage --json   (aas-cli::cmd_usage_json → aas-providers::snapshot() serialized)
        └─ {"accounts":[{provider,name,email,active,plan,headline,error,notes,
                         meters:[{label,usedPct,resetMs}]}]}
```

The `aas` binary is located as `$AAS_BIN` → `~/.local/bin/aas` → `~/bin/aas` →
`~/.cargo/bin/aas` → `/opt/homebrew`, `/usr/local`, `/usr/bin` → `/usr/bin/env aas` (PATH).
GUI apps get a minimal `PATH`, hence the explicit search.

### Why shell out (not link a Swift↔Rust bridge)
`aas usage --json` is a clean, versioned contract that keeps the Swift app decoupled from the
Rust internals and gives every other integration the same machine-readable output. The JSON
is produced by the *same* `aas-providers::snapshot()` fan-out that backs the `aas usage`
table, so the app and CLI never drift.

### Health mapping (shared with the CLI's thresholds)
`remaining = 100 − usedPct`; worst remaining across all meters drives the color: `< 10` red,
`< 30` orange, else green; any account with an `error` reads as "needs attention" (red). The
menubar ring fills with the worst *used* share.

---

## 4. Menubar ring gauge + visual language

The menubar mark is a **ring gauge**, not a flat dot: a donut whose arc fills with the worst
account's *used* share and takes the health color — so one mark encodes both *how full* and
*how urgent*. Rasterized in code to RGBA (edge-antialiased band + round caps + a neutral
track ring); no bundled image assets.

Health color (arc, gauge bars, status chip) by **worst remaining %** across every meter:

| Color | Condition |
|---|---|
| 🔴 red | any account `usage.error` is set (expired token / network), **or** worst remaining < 10% |
| 🟡 amber | worst remaining in 10–30% |
| 🟢 green | worst remaining ≥ 30% |
| ⚪️ gray | no accounts / never fetched yet |

> Glance-tuned thresholds, deliberately **not** `usage::bar_level` (90/70 for the CLI table).

**Visual language** (native macOS, both light & dark):
- **Type:** the native system font with size/weight and semantic foreground hierarchy.
- **Color:** neutral surfaces + Apple-style semantic health greens/ambers/reds; a single
  restrained system-blue **accent** used only to mark the active account.
- **Surface:** native frosted vibrancy through `NSVisualEffectView` with popover material.
- **Theme:** SwiftUI follows the current system appearance automatically.
- Provider marks are small bundled template PNGs; fallback marks use SF Symbols. Their
  trademarks remain owned by their respective providers.

---

## 5. Popover panel (read-only)

```
┌──────────────────────────────────────┐
│  aas                    updated 6:41 PM │
│  ● worst 2% left                        │
├──────────────────────────────────────┤
│  CLAUDE                                │
│  ┌──────────────────────────────────┐ │
│  │ ● k-june@callabo        max · 20x │ │
│  │ 5h ▐███▌░░░░░  38%  resets 3:26 PM │ │  ← green fill
│  │ 7d ▐████████▌ 85%  resets Jul 12  │ │  ← yellow fill
│  └──────────────────────────────────┘ │
│  ┌──────────────────────────────────┐ │
│  │   june@rtzr             team · 5x │ │
│  │ 5h ▐██████████ 100% resets 1:42…  │ │  ← red fill
│  └──────────────────────────────────┘ │
│  CODEX  …                              │
├──────────────────────────────────────┤
│  ⟳  Refresh                     Quit   │
└──────────────────────────────────────┘
```

- Header: title + `updated HH:MM` + a colored one-line status (`worst N% left` / `needs
  attention` / `healthy`).
- One **card per account**: an accent dot marks the aas-active account; plan right-aligned;
  then one SwiftUI capsule meter per quota, **filled by *used* %** (full = at limit) and
  colored green/yellow/red by remaining, with a compact reset ETA.
- An account whose `usage.error` is set shows a red `⚠ …` line; a meterless provider
  (cursor) shows its headline.
- Footer: **Refresh**, Launch at Login, and **Quit**. Empty state: centered `no accounts /
  run: aas login`.
- The card list lives in a height-constrained SwiftUI `ScrollView`, so the footer remains
  reachable for large account sets.

---

## 6. Refreshing & caching

- **On launch:** render the last successful snapshot immediately. Fetch automatically only
  when no cache has ever been written.
- **No polling:** usage endpoints are rate-limited, so the app never starts a timer.
- **Cache:** keep the last successful account list; cards remain visible during refresh and
  after failure. Header shows relative age and a visible failure/stale banner.
- **Refresh:** coalesces overlapping requests and explains the 30-second cooldown. The CLI's
  persisted per-account backoff remains the authoritative network gate.
- **Process safety:** each CLI invocation has a 90-second total deadline, a terminate/kill
  fallback, file-backed output to avoid pipe deadlocks, and bounded stderr diagnostics.

---

## 7. Milestones

| # | Milestone | Done when |
|---|---|---|
| M1 | Scaffold crate + workspace, static dot + Quit | menubar dot appears, no Dock icon, Quit exits ✓ |
| M2 | Extract `aas-providers::snapshot()`, share with CLI | `aas usage` and JSON use one snapshot path ✓ |
| M3 | SwiftUI popover: header, provider groups, account cards, bars | popover mirrors `aas usage` ✓ |
| M4 | Glance dot + status color from worst remaining | dot/header track quota; error → red ✓ |
| M5 | Cache + guarded Refresh | no polling; manual refresh, cooldown, and errors are visible ✓ |
| M6 | Portable `.app` + Launch at Login | resources resolve off-machine; strict signing passes ✓ |

---

## 8. Open questions / later

- **Switch action** (v2) turns this read-write: click a card → `aas switch`; needs
  keychain-write UX and error surfacing.
- **Threshold alerts** via `UserNotifications` (v2).
- **Signed distribution:** local builds are ad-hoc signed; a notarized Developer ID release
  remains a distribution concern.
