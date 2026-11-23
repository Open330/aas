# aas Alerts (BarShelf widget)

Notifies when any aas account runs low, and again when limits reset. A small
companion to **aas Usage**: install it only if you want notifications.

- Runs `aas usage --json` in the background every 5 minutes while its card is
  showing and about every 20 minutes otherwise. It uses the lowest remaining
  percentage among accounts with a successful quota reading.
- **Low**: notifies once when that drops to the threshold (default 20 % left).
- **Reset** notifies once when all reported percentages return to 90 % or higher.
  Accounts that successfully report without a percentage, such as Pi, are skipped.
  A fetch error blocks reset and leaves the low alert's state unchanged unless
  another account reports a percentage at or below the low threshold.
- Each alert fires on the change, not on every refresh, and stays quiet for an
  hour after it fires. Both thresholds are in the widget's settings.

Requires BarShelf 0.7.21 or later (workflow alerts) and the `aas` CLI. It asks
to run `aas usage --json` and to send notifications.
