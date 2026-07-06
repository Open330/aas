import SwiftUI
import AppKit

@main
struct AasBarApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model = UsageModel()

    var body: some Scene {
        MenuBarExtra {
            PopoverView(model: model)
                .background(VisualEffectBackground())
        } label: {
            MenuBarLabel(model: model)
        }
        .menuBarExtraStyle(.window) // rich popover, not a text menu
    }
}

/// Hide the Dock tile — this is a menubar agent, like `LSUIElement`.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        // Offline design snapshot: `AAS_BAR_SNAPSHOT=/path.png [AAS_BAR_SCHEME=light] AasBar`
        // renders the popover to a PNG and exits — lets us review the layout without clicking.
        let env = ProcessInfo.processInfo.environment
        if let path = env["AAS_BAR_SNAPSHOT"], !path.isEmpty {
            let scheme: ColorScheme = env["AAS_BAR_SCHEME"] == "light" ? .light : .dark
            renderSnapshot(to: path, scheme: scheme)
            exit(0)
        }
        NSApp.setActivationPolicy(.accessory)
    }

    @MainActor private func renderSnapshot(to path: String, scheme: ColorScheme) {
        let model = UsageModel()
        model.accounts = Account.samples
        model.updated = Date()
        let bg = scheme == .light
            ? Color(red: 0.96, green: 0.96, blue: 0.97)
            : Color(red: 0.13, green: 0.13, blue: 0.145)
        let content = ZStack {
            bg
            PopoverView(model: model)
        }
        .frame(width: 300)
        .environment(\.colorScheme, scheme)
        let renderer = ImageRenderer(content: content)
        renderer.scale = 2.0
        guard let image = renderer.nsImage,
              let tiff = image.tiffRepresentation,
              let rep = NSBitmapImageRep(data: tiff),
              let png = rep.representation(using: .png, properties: [:]) else { return }
        try? png.write(to: URL(fileURLWithPath: path))
    }
}

/// The menubar mark: a ring gauge that fills with the worst account's usage, health-colored.
/// Rendered as an `NSImage` (not a SwiftUI Shape) — a custom-drawn view doesn't reliably size
/// a `MenuBarExtra` status item, but an image label always does.
struct MenuBarLabel: View {
    @ObservedObject var model: UsageModel

    var body: some View {
        let summary = summarize(model.accounts)
        Image(nsImage: ringImage(fraction: summary.fraction, color: summary.level.nsColor))
            .onAppear { model.start() }
    }
}

/// A ring gauge drawn with AppKit: a faint full track + a health-colored arc sweeping
/// clockwise from the top by `fraction`. `isTemplate = false` keeps the health color.
func ringImage(fraction: Double, color: NSColor) -> NSImage {
    let size = NSSize(width: 18, height: 18)
    let image = NSImage(size: size, flipped: false) { rect in
        let lineWidth: CGFloat = 2.4
        let center = NSPoint(x: rect.midX, y: rect.midY)
        let radius = (min(rect.width, rect.height) - lineWidth) / 2 - 0.5

        let track = NSBezierPath()
        track.appendArc(withCenter: center, radius: radius, startAngle: 0, endAngle: 360)
        track.lineWidth = lineWidth
        NSColor(white: 0.55, alpha: 0.65).setStroke()
        track.stroke()

        let frac = max(0, min(1, fraction))
        if frac > 0.001 {
            let arc = NSBezierPath()
            arc.appendArc(
                withCenter: center, radius: radius,
                startAngle: 90, endAngle: 90 - CGFloat(frac) * 360, clockwise: true
            )
            arc.lineWidth = lineWidth
            arc.lineCapStyle = .round
            color.setStroke()
            arc.stroke()
        }
        return true
    }
    image.isTemplate = false
    return image
}
