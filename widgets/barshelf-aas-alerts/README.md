# aas Alerts (BarShelf widget)

Notifies when any aas account runs low, and again when limits reset. A small
companion to **aas Usage**: install it only if you want notifications.

- Runs `aas usage --json` in the background (every 5 minutes, relaxed while the
  BarShelf popup is closed) and looks at `worstRemainingPct`, the least left
  across all accounts.
- **Low**: notifies once when that drops to the threshold (default 20 % left).
- **Back**: notifies once when every account is above 90 % again, i.e. after a reset.
- Each alert fires on the change, not on every refresh, and stays quiet for an
  hour after it fires. Both thresholds are in the widget's settings.

Requires BarShelf 0.7.21 or later (workflow alerts) and the `aas` CLI. It asks
to run `aas usage --json` and to send notifications.
