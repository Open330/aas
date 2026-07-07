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

// MARK: - Account row (subtle card, no border)

struct AccountRow: View {
    let account: Account

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                if account.active {
                    Circle().fill(Color.accentColor).frame(width: 5, height: 5)
                }
                Text(account.name).font(.system(size: 12.5, weight: .semibold))
                Spacer(minLength: 8)
                if let plan = account.plan, !plan.isEmpty {
                    Text(plan)
                        .font(.system(size: 10, weight: .medium))
                        .foregroundStyle(.tertiary)
                }
            }

            if let error = account.error {
                Text(error)
                    .font(.system(size: 10.5))
                    .foregroundStyle(.red)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            } else if account.meters.isEmpty {
                Text(account.headline)
                    .font(.system(size: 10.5))
                    .foregroundStyle(.tertiary)
            } else {
                VStack(spacing: 5) {
                    ForEach(account.meters) { meter in
                        MeterRow(meter: meter)
                    }
                }
            }
        }
        .padding(.vertical, 7)
        .padding(.horizontal, 12)
        .background(
            RoundedRectangle(cornerRadius: 11, style: .continuous)
                .fill(account.active ? Color.accentColor.opacity(0.10) : Color.primary.opacity(0.05))
        )
    }
}

// MARK: - Meter row (aligned columns)

struct MeterRow: View {
    let meter: Meter

    var body: some View {
        HStack(spacing: 9) {
            Text(meter.label)
                .font(.system(size: 10.5, weight: .medium, design: .monospaced))
                .foregroundStyle(.secondary)
                .frame(width: 18, alignment: .leading)

            GeometryReader { geo in
                Capsule()
                    .fill(Color.primary.opacity(0.10))
                    .overlay(alignment: .leading) {
                        Capsule()
                            .fill(meterColor(usedPct: meter.usedPct))
                            .frame(width: max(3, geo.size.width * CGFloat(min(1, meter.usedPct / 100))))
                    }
            }
            .frame(height: 6)

            Text("\(Int(meter.usedPct.rounded()))%")
                .font(.system(size: 11.5, weight: .semibold))
                .monospacedDigit()
                .lineLimit(1)
                .fixedSize()
                .frame(width: 40, alignment: .trailing)

            if let eta = shortEta(meter.resetMs) {
                Text(eta)
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
                    .monospacedDigit()
                    .frame(width: 42, alignment: .trailing)
            }
        }
    }
}
