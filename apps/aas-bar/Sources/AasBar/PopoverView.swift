import SwiftUI
import ServiceManagement

/// Native frosted (vibrancy) background for the popover — the "melts into macOS" material.
struct VisualEffectBackground: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = .popover
        view.blendingMode = .behindWindow
        view.state = .active
        return view
    }
    func updateNSView(_ nsView: NSVisualEffectView, context: Context) {}
}

struct PopoverView: View {
    @ObservedObject var model: UsageModel
    @State private var loginEnabled = SMAppService.mainApp.status == .enabled

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().opacity(0.6)
            content
            Divider().opacity(0.6)
            footer
        }
        .frame(width: 300)
    }

    // MARK: Header

    private var header: some View {
        let summary = summarize(model.accounts)
        return HStack(spacing: 6) {
            Text("aas").font(.system(size: 14, weight: .bold))
            Circle().fill(summary.level.color).frame(width: 6, height: 6).padding(.leading, 2)
            Text(summary.text).font(.system(size: 11)).foregroundStyle(.secondary)
            Spacer(minLength: 6)
            if let updated = model.updated {
                Text(relativeTime(updated))
                    .font(.system(size: 10.5)).foregroundStyle(.tertiary)
            }
        }
        .padding(.horizontal, 14)
        .padding(.top, 10)
        .padding(.bottom, 8)
    }

    // MARK: Content

    @ViewBuilder
    private var content: some View {
        if model.accounts.isEmpty {
            VStack(spacing: 8) {
                if model.updated == nil && model.loadError == nil {
                    ProgressView().controlSize(.small)
                    Text("Loading…").font(.system(size: 12)).foregroundStyle(.secondary)
                } else {
                    Text(model.loadError == nil ? "No accounts yet" : "Couldn’t load usage")
                        .font(.system(size: 12.5, weight: .medium))
                        .foregroundStyle(.secondary)
                    Text(model.loadError ?? "run  aas login  to add one")
                        .font(.system(size: 11))
                        .foregroundStyle(.tertiary)
                        .multilineTextAlignment(.center)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 40)
            .padding(.horizontal, 16)
        } else {
            VStack(alignment: .leading, spacing: 11) {
                ForEach(providerOrder, id: \.self) { provider in
                    VStack(alignment: .leading, spacing: 6) {
                        Text(displayProvider(provider).uppercased())
                            .font(.system(size: 10, weight: .semibold))
                            .tracking(0.7)
                            .foregroundStyle(.tertiary)
                            .padding(.leading, 2)
                        ForEach(accounts(for: provider)) { account in
                            AccountRow(account: account)
                        }
                    }
                }
            }
            .padding(.horizontal, 14)
            .padding(.top, 10)
            .padding(.bottom, 11)
        }
    }

    /// Accounts for a provider, most urgent first (errors, then tightest remaining).
    private func accounts(for provider: String) -> [Account] {
        model.accounts
            .filter { $0.provider == provider }
            .sorted { $0.urgency < $1.urgency }
    }

    private var providerOrder: [String] {
        var seen = Set<String>()
        var order = [String]()
        for account in model.accounts where seen.insert(account.provider).inserted {
            order.append(account.provider)
        }
        return order
    }

    // MARK: Footer

    private var footer: some View {
        HStack(spacing: 8) {
            Button(action: { model.refresh() }) {
                HStack(spacing: 5) {
                    Image(systemName: "arrow.clockwise")
                    Text("Refresh")
                }
                .font(.system(size: 11.5))
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)

            if model.loading { ProgressView().controlSize(.small) }
            Spacer()
            Menu {
                Button(action: toggleLogin) {
                    if loginEnabled {
                        Label("Launch at Login", systemImage: "checkmark")
                    } else {
                        Text("Launch at Login")
                    }
                }
                Divider()
                Button("Quit aas-bar") { NSApp.terminate(nil) }
            } label: {
                Image(systemName: "ellipsis.circle").font(.system(size: 13))
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 13)
        .padding(.vertical, 8)
    }

    private func toggleLogin() {
        do {
            if SMAppService.mainApp.status == .enabled {
                try SMAppService.mainApp.unregister()
            } else {
                try SMAppService.mainApp.register()
            }
        } catch {
            NSLog("aas-bar: couldn't toggle Launch at Login — \(error.localizedDescription)")
        }
        loginEnabled = SMAppService.mainApp.status == .enabled
    }
}

// MARK: - Account row — identity on the left, ring gauges on the right

struct AccountRow: View {
    let account: Account

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 5) {
                    if account.active {
                        Circle().fill(Color.accentColor).frame(width: 5, height: 5)
                    }
                    Text(account.name)
                        .font(.system(size: 12.5, weight: .semibold))
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                if let plan = account.plan, !plan.isEmpty {
                    Text(plan.uppercased())
                        .font(.system(size: 9, weight: .semibold))
                        .tracking(0.5)
                        .foregroundStyle(.tertiary)
                }
                if account.error == nil, let eta = tightestReset {
                    Text("↺ \(eta)")
                        .font(.system(size: 10))
                        .foregroundStyle(.quaternary)
                        .monospacedDigit()
                }
            }

            Spacer(minLength: 6)

            if account.error != nil {
                Text(compactError)
                    .font(.system(size: 10.5, weight: .medium))
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.trailing)
                    .lineLimit(2)
                    .frame(maxWidth: 110, alignment: .trailing)
            } else if account.meters.isEmpty {
                Text(account.headline)
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
                    .lineLimit(2)
                    .frame(maxWidth: 120, alignment: .trailing)
            } else {
                HStack(spacing: 12) {
                    ForEach(account.meters) { meter in
                        RingMeter(label: meter.label, usedPct: meter.usedPct)
                    }
                }
            }
        }
        .padding(.vertical, 9)
        .padding(.horizontal, 12)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(account.active ? Color.accentColor.opacity(0.10) : Color.primary.opacity(0.045))
        )
    }

    /// Reset time of the tightest (most-used) meter — the limit you'll hit first.
    private var tightestReset: String? {
        guard let m = account.meters.max(by: { $0.usedPct < $1.usedPct }) else { return nil }
        return shortEta(m.resetMs)
    }

    private var compactError: String {
        let l = (account.error ?? "").lowercased()
        if l.contains("expired") || l.contains("401") { return "token expired" }
        if l.contains("rate limit") || l.contains("429") || l.contains("backing off") { return "rate limited" }
        if l.contains("network") { return "network error" }
        return "unavailable"
    }
}

// MARK: - Ring gauge (per meter, arc fills with used %)

struct RingMeter: View {
    let label: String
    let usedPct: Double

    var body: some View {
        VStack(spacing: 4) {
            ZStack {
                Circle()
                    .stroke(Color.primary.opacity(0.10), lineWidth: 3.5)
                Circle()
                    .trim(from: 0, to: max(0.001, min(1, usedPct / 100)))
                    .stroke(meterColor(usedPct: usedPct), style: StrokeStyle(lineWidth: 3.5, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                Text("\(Int(usedPct.rounded()))")
                    .font(.system(size: 12, weight: .semibold))
                    .monospacedDigit()
            }
            .frame(width: 40, height: 40)
            Text(label)
                .font(.system(size: 9, weight: .medium, design: .monospaced))
                .foregroundStyle(.secondary)
        }
    }
}
