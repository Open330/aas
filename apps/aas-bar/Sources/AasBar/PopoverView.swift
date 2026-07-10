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
    var showFooter = true
    @State private var loginEnabled = SMAppService.mainApp.status == .enabled
    @State private var loginError: String?
    @State private var showingLoginError = false

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().opacity(0.6)
            content
            if showFooter {
                Divider().opacity(0.6)
                footer
            }
        }
        .frame(width: 300)
        .alert("Launch at Login failed", isPresented: $showingLoginError) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(loginError ?? "The setting could not be changed.")
        }
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
            VStack(spacing: 0) {
                if let error = model.loadError {
                    StatusBanner(text: "Refresh failed · \(error)", color: .red)
                } else if let notice = model.refreshNotice {
                    StatusBanner(text: notice, color: .secondary)
                }
                ScrollView {
                    VStack(alignment: .leading, spacing: 11) {
                        ForEach(providerOrder, id: \.self) { provider in
                            VStack(alignment: .leading, spacing: 5) {
                                HStack(spacing: 5) {
                                    ProviderMark(provider: provider)
                                    Text(displayProvider(provider).uppercased())
                                        .font(.system(size: 10, weight: .semibold))
                                        .tracking(0.7)
                                        .foregroundStyle(.tertiary)
                                }
                                .padding(.leading, 2)
                                ForEach(accounts(for: provider)) { account in
                                    AccountRow(account: account)
                                }
                            }
                        }
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                }
                .frame(maxHeight: 480)
            }
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
            Button(action: { model.loading ? model.cancelRefresh() : model.refresh() }) {
                HStack(spacing: 5) {
                    Image(systemName: model.loading ? "xmark" : "arrow.clockwise")
                    Text(model.loading ? "Cancel" : "Refresh")
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
            loginError = error.localizedDescription
            showingLoginError = true
        }
        loginEnabled = SMAppService.mainApp.status == .enabled
    }
}

struct StatusBanner: View {
    let text: String
    let color: Color

    var body: some View {
        Text(text)
            .font(.system(size: 10.5, weight: .medium))
            .foregroundStyle(color)
            .lineLimit(2)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 14)
            .padding(.vertical, 7)
            .background(color.opacity(0.08))
    }
}

// MARK: - Provider mark (real brand logo, or an SF Symbol fallback)

struct ProviderMark: View {
    let provider: String

    var body: some View {
        Group {
            if let logo = providerLogo(provider) {
                Image(nsImage: logo)
                    .renderingMode(.template)
                    .resizable()
                    .scaledToFit()
            } else {
                Image(systemName: providerSymbol(provider))
                    .resizable()
                    .scaledToFit()
            }
        }
        .frame(width: 12, height: 12)
        .foregroundStyle(providerColor(provider))
    }
}

// MARK: - Account row — compact, linear meters

struct AccountRow: View {
    let account: Account

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                // Colour signals active (accent) vs inactive (muted); always present so names align.
                Circle()
                    .fill(account.active ? Color.accentColor : Color.secondary.opacity(0.4))
                    .frame(width: 6, height: 6)
                Text(account.name)
                    .font(.system(size: 12.5, weight: .semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 8)
                if let plan = account.planChip {
                    Text(plan)
                        .font(.system(size: 8.5, weight: .semibold))
                        .tracking(0.4)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .fixedSize()
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Capsule().fill(Color.primary.opacity(0.07)))
                }
            }

            if account.error != nil {
                Text(compactError)
                    .font(.system(size: 10.5, weight: .medium))
                    .foregroundStyle(.red)
                    .lineLimit(1)
            } else if account.meters.isEmpty {
                Text(account.headline)
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
            } else {
                VStack(spacing: 3) {
                    ForEach(account.meters) { meter in
                        LinearMeter(meter: meter)
                    }
                }
            }
        }
        .padding(.vertical, 7)
        .padding(.horizontal, 12)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(Color.primary.opacity(0.045))
        )
    }

    private var compactError: String {
        let l = (account.error ?? "").lowercased()
        if l.contains("expired") || l.contains("401") { return "token expired" }
        if l.contains("rate limit") || l.contains("429") || l.contains("backing off") { return "rate limited" }
        if l.contains("network") { return "network error" }
        return "unavailable"
    }
}

// MARK: - Linear meter (compact horizontal gauge)

struct LinearMeter: View {
    let meter: Meter

    var body: some View {
        HStack(spacing: 8) {
            Text(meter.label)
                .font(.system(size: 9.5, weight: .medium, design: .monospaced))
                .foregroundStyle(.secondary)
                .frame(width: 16, alignment: .leading)

            GeometryReader { geo in
                Capsule()
                    .fill(Color.primary.opacity(0.09))
                    .overlay(alignment: .leading) {
                        Capsule()
                            .fill(meterColor(usedPct: meter.usedPct))
                            .frame(width: max(4, geo.size.width * CGFloat(min(1, meter.usedPct / 100))))
                    }
            }
            .frame(height: 5)

            Text("\(Int(meter.usedPct.rounded()))%")
                .font(.system(size: 11, weight: .semibold))
                .monospacedDigit()
                .frame(width: 34, alignment: .trailing)

            if let eta = shortEta(meter.resetMs) {
                Text(eta)
                    .font(.system(size: 9.5))
                    .foregroundStyle(.quaternary)
                    .monospacedDigit()
                    .frame(width: 44, alignment: .trailing)
            }
        }
    }
}
