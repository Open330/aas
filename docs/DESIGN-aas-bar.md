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
1. **Glance in the menubar.** One colored dot (🟢/🟡/🔴) summarizes the *worst* remaining
   quota across all accounts. No terminal, no waiting.
2. **Details on click.** A dropdown lists each account grouped by provider, with an ASCII
   meter bar, used-%, and reset time — the same data `aas usage` shows.
3. **Reuse the core.** Fetch through `aas-providers` in-process. No re-implementation of
   provider logic, no dependency on the installed `aas` binary's version, no text parsing.
4. **Single binary, project ethos.** A Cargo-workspace crate, one executable, matching
   `aas`'s dependency-light philosophy. No Electron/webview runtime, no separate toolchain.
5. **Cheap to run.** Idle in the menubar; fetch on an interval with a cache so the menu is
   always populated and the network is hit sparingly.

### Non-Goals (v1)
- **No mutations.** Read-only. No `switch`, no `login`, no `exec`. (v2.)
- **No notifications / threshold alerts.** (v2.)
- **No launch-at-login / `.app` bundle** in the first cut — a plain binary that hides its
   Dock icon at runtime. Bundling comes with distribution. (v2.)
- **Non-macOS.** Windows/Linux tray is possible with the same crates later, not now.

> **UI history.** v1 first shipped as a plain `muda` text menu (ASCII bars). That wasted the
> GUI surface, so it was replaced with an egui-rendered popover (colored bars, cards). A
> SwiftUI popover would look more native but leaves the Rust workspace; egui keeps one binary.

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
        ├─ Timer(180s) + refresh() → runs `aas usage --json` (Process, off main thread)
        └─ JSONDecoder → [Account]; @Published drives the label + popover
                    │
                    ▼  shells out to
  aas usage --json   (aas-cli::cmd_usage_json → aas-providers::snapshot() serialized)
        └─ {"accounts":[{provider,name,email,active,plan,headline,error,
                         meters:[{label,usedPct,resetMs}]}]}
```

The `aas` binary is located as `$AAS_BIN` → `~/.cargo/bin/aas` → `/opt/homebrew`,
`/usr/local`, `/usr/bin` → `/usr/bin/env aas` (PATH). GUI apps get a minimal `PATH`, hence
the explicit search.

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
- **Type:** Inter (Regular/Medium/SemiBold, bundled OFL) as an SF-Pro substitute; hierarchy
  by size + weight + a tuned secondary/tertiary text opacity.
- **Color:** neutral surfaces + Apple-style semantic health greens/ambers/reds; a single
  restrained system-blue **accent** used only to mark the active account.
- **Surface:** frosted **vibrancy** — a transparent window backed by an `NSVisualEffectView`
  (`Popover` material, `BehindWindow`), with a translucent egui fill on top. `AAS_BAR_SOLID=1`
  falls back to an opaque tuned surface (identical layout) if vibrancy misbehaves.
- **Theme:** follows the system appearance via `window.theme()` + `WindowEvent::ThemeChanged`.
- Cards, hairlines, and slim gauge bars are custom-painted — no default egui chrome.

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
- One **card per account**: `●` (green) marks the aas-active account; plan right-aligned;
  then one `egui::ProgressBar` per meter, **filled by *used* %** (full = at limit) and
  colored green/yellow/red by remaining, with `usage::format_reset` text.
- An account whose `usage.error` is set shows a red `⚠ …` line; a meterless provider
  (cursor) shows its headline.
- Footer: **Refresh** (force fetch) and **Quit**. Empty state: centered `no accounts /
  run: aas login`.
- The card list lives in a vertical `ScrollArea`, so any number of accounts fits the fixed
  340×460 window.

---

## 6. Polling & caching

- **On launch:** one fetch immediately; render from the result.
- **Interval:** re-fetch every **180s** by default (`AAS_BAR_INTERVAL_SECS` to override).
- **Cache:** keep the last successful `Vec<AccountUsage>`; the menu and dot never blank out
  during an in-flight fetch or a failure. Header shows `updated Ns ago`.
- **Refresh:** forces an immediate fetch; ignored (coalesced) if one is already in flight.

---

## 7. Milestones

| # | Milestone | Done when |
|---|---|---|
| M1 | Scaffold crate + workspace, static dot + Quit | menubar dot appears, no Dock icon, Quit exits ✓ |
| M2 | Extract `aas-providers::snapshot()`, share with CLI | `aas usage` on snapshot path; 90 tests green ✓ |
| M3 | egui popover: header, provider groups, account cards, bars | popover mirrors `aas usage` ✓ |
| M4 | Glance dot + status color from worst remaining | dot/header track quota; error → red ✓ |
| M5 | Fetch thread + interval + Refresh button | auto-refresh; manual refresh works ✓ |
| M6 | Popover show/hide: position, focus-loss, debounce toggle | opens under icon, dismisses cleanly ✓ |
| — | *(v2)* `.app` bundle, launch-at-login, Switch action, alerts | later |

---

## 8. Open questions / later

- **`.app` bundling & signing** for double-click launch and login items (v2).
- **Switch action** (v2) turns this read-write: click a card → `aas switch`; needs
  keychain-write UX and error surfacing.
- **Threshold alerts** via `UserNotifications` (v2).
- **Popover polish**: rounded/translucent window (needs a transparent, shaped NSWindow),
  arrow anchor, and a spinner while a fetch is in flight.
- **Focus-loss reliability** under `Accessory` activation — the debounce toggle is a
  heuristic; if it ever mis-fires, anchor hide/show on an explicit toggle timestamp.
