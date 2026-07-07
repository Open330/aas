import SwiftUI
import AppKit

// MARK: - Wire format (matches `aas usage --json`)

struct UsageResponse: Codable {
    let accounts: [Account]
}

/// On-disk snapshot shown on launch until the user refreshes.
struct CachedUsage: Codable {
    let accounts: [Account]
    let updatedAt: Date
}

struct Account: Codable, Identifiable {
    let provider: String
    let name: String
    let email: String?
    let active: Bool
    let plan: String?
    let planLabel: String?
    let headline: String
    let error: String?
    let meters: [Meter]

    var id: String { "\(provider)/\(name)" }

    /// Chip text: the detailed plan ("max · 20x"), base uppercased, suffix kept.
    var planChip: String? {
        let raw = (planLabel ?? plan).flatMap { $0.isEmpty ? nil : $0 }
        guard let raw else { return nil }
        if let dot = raw.firstIndex(of: "·") {
            let base = raw[..<dot].trimmingCharacters(in: .whitespaces).uppercased()
            let suffix = raw[raw.index(after: dot)...].trimmingCharacters(in: .whitespaces)
            return suffix.isEmpty ? base : "\(base) · \(suffix)"
        }
        return raw.uppercased()
    }
}

struct Meter: Codable, Identifiable {
    let label: String
    let usedPct: Double
    let resetMs: Int64?

    var id: String { label }
    var remaining: Double { max(0, min(100, 100 - usedPct)) }
}

extension Account {
    /// Lower = more urgent (sorts to the top). Errored accounts first, then by the tightest
    /// remaining quota; meterless accounts (e.g. cursor) sink to the bottom.
    var urgency: Double {
        if error != nil { return -1 }
        return meters.map(\.remaining).min() ?? 200
    }

    /// Representative data for the offline snapshot renderer (see AAS_BAR_SNAPSHOT).
    /// Deliberately out of urgency order to exercise the sort.
    static var samples: [Account] {
        let now = Date().timeIntervalSince1970 * 1000
        func inH(_ h: Double) -> Int64 { Int64(now + h * 3600 * 1000) }
        func m(_ label: String, _ used: Double, _ h: Double) -> Meter { Meter(label: label, usedPct: used, resetMs: inH(h)) }
        return [
            Account(provider: "claude", name: "e-ed@callabo", email: nil, active: false, plan: "max", planLabel: "max · 20x", headline: "", error: nil, meters: [m("5h", 12, 2), m("7d", 21, 131)]),
            Account(provider: "claude", name: "k-june@callabo", email: nil, active: true, plan: "max", planLabel: "max · 20x", headline: "", error: nil, meters: [m("5h", 38, 1.5), m("7d", 85, 51)]),
            Account(provider: "claude", name: "june@rtzr", email: "june@rtzr.ai", active: false, plan: "team", planLabel: "team · 5x", headline: "", error: nil, meters: [m("5h", 100, 1.7), m("7d", 98, 4.4)]),
            Account(provider: "codex", name: "personal.codex", email: nil, active: false, plan: "plus", planLabel: "plus", headline: "", error: nil, meters: [m("5h", 5, 4), m("7d", 24, 142)]),
            Account(provider: "codex", name: "e-ed.codex", email: nil, active: true, plan: "pro", planLabel: "pro", headline: "", error: nil, meters: [m("5h", 7, 2), m("7d", 93, 16)]),
        ]
    }
}

// MARK: - Health → color

/// Semantic health, mapped to both SwiftUI and AppKit colors so the popover (SwiftUI) and the
/// menubar image (AppKit) stay in sync.
enum HealthLevel {
    case good, warn, bad, none

    var color: Color {
        switch self {
        case .good: return .green
        case .warn: return .orange
        case .bad: return .red
        case .none: return .secondary
        }
    }

    var nsColor: NSColor {
        switch self {
        case .good: return .systemGreen
        case .warn: return .systemOrange
        case .bad: return .systemRed
        case .none: return .secondaryLabelColor
        }
    }
}

func meterLevel(usedPct: Double) -> HealthLevel {
    let remaining = 100 - usedPct
    if remaining < 10 { return .bad }
    if remaining < 30 { return .warn }
    return .good
}

func meterColor(usedPct: Double) -> Color { meterLevel(usedPct: usedPct).color }

struct Summary {
    let fraction: Double // menubar ring fill = worst account's *used* share
    let level: HealthLevel
    let text: String
}

/// Roll every account up into the single menubar mark: fill = worst usage, color + text by
/// the tightest remaining quota (an errored account dominates as "needs attention").
func summarize(_ accounts: [Account]) -> Summary {
    if accounts.isEmpty {
        return Summary(fraction: 0, level: .none, text: "no accounts")
    }
    var worstRemaining = 100.0
    var sawMeter = false
    var sawError = false
    for account in accounts {
        if account.error != nil { sawError = true }
        for meter in account.meters {
            worstRemaining = min(worstRemaining, meter.remaining)
            sawMeter = true
        }
    }
    let usedFraction = sawMeter ? (100 - worstRemaining) / 100 : 0

    if sawError {
        return Summary(fraction: sawMeter ? usedFraction : 1.0, level: .bad, text: "needs attention")
    }
    let level: HealthLevel = worstRemaining < 10 ? .bad : (worstRemaining < 30 ? .warn : .good)
    let text = sawMeter ? "worst \(Int(worstRemaining.rounded()))% left" : "healthy"
    return Summary(fraction: usedFraction, level: level, text: text)
}

func displayProvider(_ id: String) -> String {
    switch id {
    case "claude": return "Claude"
    case "codex": return "Codex"
    case "grok": return "Grok"
    case "zai": return "Z.AI"
    case "cursor": return "Cursor"
    default: return id
    }
}

/// Bundled brand logo (template PNG) for a provider, if present in Resources.
func providerLogo(_ id: String) -> NSImage? {
    guard let url = Bundle.module.url(forResource: id, withExtension: "png"),
          let img = NSImage(contentsOf: url) else { return nil }
    img.isTemplate = true
    return img
}

/// SF Symbol fallback when no bundled logo exists for a provider.
func providerSymbol(_ id: String) -> String {
    switch id {
    case "zai": return "z.circle.fill"
    default: return "circle.fill"
    }
}

/// Tint per provider — coral for Anthropic, otherwise the adaptive label color
/// (the real OpenAI / X marks are monochrome).
func providerColor(_ id: String) -> Color {
    switch id {
    case "claude": return Color(red: 0.85, green: 0.47, blue: 0.36) // Anthropic coral
    case "zai": return Color(red: 0.55, green: 0.45, blue: 0.95)
    default: return .primary.opacity(0.85)
    }
}

/// Relative "updated" label, e.g. "just now", "3 min ago", "5 hr ago" — makes stale cache obvious.
func relativeTime(_ date: Date) -> String {
    let seconds = Date().timeIntervalSince(date)
    if seconds < 45 { return "just now" }
    let fmt = RelativeDateTimeFormatter()
    fmt.unitsStyle = .abbreviated
    return fmt.localizedString(for: date, relativeTo: Date())
}

/// Compact "time left" from an epoch-ms reset, e.g. "1h 32m", "51h", "now".
func shortEta(_ ms: Int64?) -> String? {
    guard let ms = ms else { return nil }
    let now = Date().timeIntervalSince1970 * 1000
    let diff = Double(ms) - now
    if diff <= 0 { return "now" }
    let mins = Int((diff / 60000).rounded())
    let (h, m) = (mins / 60, mins % 60)
    if h >= 100 { return "\(h)h" }
    if h > 0 { return m > 0 ? "\(h)h \(m)m" : "\(h)h" }
    return "\(m)m"
}

// MARK: - Model

/// Runs `aas usage --json` on an interval (and on demand) and publishes the parsed accounts.
@MainActor
final class UsageModel: ObservableObject {
    @Published var accounts: [Account] = []
    @Published var updated: Date?
    @Published var loading = false
    @Published var loadError: String?

    private var started = false

    /// On first appearance: show the cached snapshot immediately (no network). Only bootstrap
    /// a fetch if there's nothing cached yet — otherwise we never hit the API on our own; the
    /// user drives it with Refresh. This keeps us from hammering the rate-limited usage API.
    func start() {
        guard !started else { return }
        started = true
        loadCache()
        if accounts.isEmpty && loadError == nil {
            refresh()
        }
    }

    func refresh() {
        Task { await fetch() }
    }

    private func fetch() async {
        loading = true
        defer { loading = false }
        do {
            let data = try await Self.runAas()
            let decoded = try JSONDecoder().decode(UsageResponse.self, from: data)
            accounts = decoded.accounts
            updated = Date()
            loadError = nil
            saveCache()
        } catch {
            loadError = error.localizedDescription
        }
    }

    // MARK: Cache (survives relaunch; shown until the user hits Refresh)

    private static func cacheURL() -> URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        let dir = base.appendingPathComponent("aas-bar", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("usage-cache.json")
    }

    private func loadCache() {
        guard let data = try? Data(contentsOf: Self.cacheURL()),
              let cached = try? JSONDecoder().decode(CachedUsage.self, from: data)
        else { return }
        accounts = cached.accounts
        updated = cached.updatedAt
    }

    private func saveCache() {
        let payload = CachedUsage(accounts: accounts, updatedAt: updated ?? Date())
        if let data = try? JSONEncoder().encode(payload) {
            try? data.write(to: Self.cacheURL())
        }
    }

    /// Locate the `aas` binary: `AAS_BIN` override, then common install dirs, then `PATH`.
    private nonisolated static func aasCommand() -> (url: URL, args: [String]) {
        let env = ProcessInfo.processInfo.environment
        if let override = env["AAS_BIN"], !override.isEmpty {
            return (URL(fileURLWithPath: override), ["usage", "--json"])
        }
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let candidates = [
            "\(home)/.cargo/bin/aas",
            "/opt/homebrew/bin/aas",
            "/usr/local/bin/aas",
            "/usr/bin/aas",
        ]
        for path in candidates where FileManager.default.isExecutableFile(atPath: path) {
            return (URL(fileURLWithPath: path), ["usage", "--json"])
        }
        // Fall back to PATH resolution.
        return (URL(fileURLWithPath: "/usr/bin/env"), ["aas", "usage", "--json"])
    }

    private nonisolated static func runAas() async throws -> Data {
        try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let (url, args) = aasCommand()
                let process = Process()
                process.executableURL = url
                process.arguments = args
                let stdout = Pipe()
                process.standardOutput = stdout
                process.standardError = Pipe()
                do {
                    try process.run()
                    let data = stdout.fileHandleForReading.readDataToEndOfFile()
                    process.waitUntilExit()
                    if process.terminationStatus != 0 {
                        continuation.resume(throwing: NSError(
                            domain: "aas", code: Int(process.terminationStatus),
                            userInfo: [NSLocalizedDescriptionKey: "aas exited with code \(process.terminationStatus)"]
                        ))
                    } else {
                        continuation.resume(returning: data)
                    }
                } catch {
                    continuation.resume(throwing: NSError(
                        domain: "aas", code: -1,
                        userInfo: [NSLocalizedDescriptionKey: "couldn't run aas — is it installed? (\(error.localizedDescription))"]
                    ))
                }
            }
        }
    }
}
