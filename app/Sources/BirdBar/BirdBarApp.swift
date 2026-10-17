import AppKit
import SwiftUI

@main
struct BirdBarApp: App {
    @StateObject private var model = BirdModel()

    init() {
        // `Bird --snapshot-window sites window.png`: render a pane of the main
        // window to an image and exit (for checking layouts without clicking).
        let args = CommandLine.arguments
        if let i = args.firstIndex(of: "--snapshot-icon"), i + 1 < args.count {
            // Both icons side by side, on light and dark (to check the dot).
            let out = URL(fileURLWithPath: args[i + 1])
            Snapshot.icons(to: out)
            exit(0)
        }
        if let i = args.firstIndex(of: "--snapshot-update-alert"), i + 1 < args.count {
            let out = URL(fileURLWithPath: args[i + 1])
            Task { @MainActor in
                await Snapshot.updateAlert(to: out)
                exit(0)
            }
        }
        if let i = args.firstIndex(of: "--snapshot-window"), i + 2 < args.count {
            let pane = Pane(rawValue: args[i + 1]) ?? .sites
            let out = URL(fileURLWithPath: args[i + 2])
            Task { @MainActor in
                await Snapshot.render(to: out, pane: pane)
                exit(0)
            }
        }
    }

    var body: some Scene {
        MenuBarExtra {
            MenuContent(model: model)
        } label: {
            // Filled while Bird runs, outlined when it is stopped; a dot when
            // an update is waiting.
            Image(nsImage: MenuBarIcon.image(running: model.state?.running == true, dot: model.updateWaiting))
        }
        .menuBarExtraStyle(.menu)

        Window("Bird", id: "main") {
            MainWindow(model: model)
        }
        .defaultSize(width: 940, height: 600)
    }
}

enum MenuBarIcon {
    /// The bird symbol, with a small dot in the top right corner when an
    /// update is waiting. A template image, so it follows the menu bar's
    /// light/dark appearance.
    static func image(running: Bool, dot: Bool) -> NSImage {
        let config = NSImage.SymbolConfiguration(pointSize: 15, weight: .regular)
        let symbol = NSImage(systemSymbolName: running ? "bird.fill" : "bird", accessibilityDescription: "Bird")?
            .withSymbolConfiguration(config) ?? NSImage()
        guard dot else {
            symbol.isTemplate = true
            return symbol
        }
        // Room on the right for the dot, clear of the bird.
        let size = NSSize(width: symbol.size.width + 6, height: max(symbol.size.height, 16))
        let image = NSImage(size: size, flipped: false) { rect in
            symbol.draw(in: NSRect(
                x: 0, y: (rect.height - symbol.size.height) / 2,
                width: symbol.size.width, height: symbol.size.height))
            let d: CGFloat = 5.5
            NSColor.black.setFill()
            NSBezierPath(ovalIn: NSRect(x: rect.width - d, y: rect.height - d, width: d, height: d)).fill()
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = "Bird, update available"
        return image
    }
}

@MainActor
enum Snapshot {
    static func icons(to url: URL) {
        let icons = [MenuBarIcon.image(running: true, dot: false), MenuBarIcon.image(running: true, dot: true)]
        let image = NSImage(size: NSSize(width: 120, height: 60), flipped: false) { _ in
            for (row, (bg, fg)) in [(NSColor.white, NSColor.black), (NSColor.black, NSColor.white)].enumerated() {
                bg.setFill()
                NSRect(x: 0, y: CGFloat(row) * 30, width: 120, height: 30).fill()
                for (col, icon) in icons.enumerated() {
                    let tinted = NSImage(size: icon.size, flipped: false) { r in
                        icon.draw(in: r)
                        fg.set()
                        r.fill(using: .sourceAtop)
                        return true
                    }
                    tinted.draw(at: NSPoint(x: 20 + col * 50, y: row * 30 + 5), from: .zero, operation: .sourceOver, fraction: 1)
                }
            }
            return true
        }
        if let tiff = image.tiffRepresentation, let rep = NSBitmapImageRep(data: tiff) {
            try? rep.representation(using: .png, properties: [:])?.write(to: url)
        }
    }

    static func updateAlert(to url: URL) async {
        let model = BirdModel()
        await model.checkForUpdates()
        guard let alert = model.makeUpdateAlert() else {
            print("no update available to show")
            return
        }
        alert.layout()
        guard let view = alert.window.contentView,
              let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return }
        view.cacheDisplay(in: view.bounds, to: rep)
        try? rep.representation(using: .png, properties: [:])?.write(to: url)
    }

    static func render(to url: URL, pane: Pane) async {
        let model = BirdModel()
        await model.refresh()
        model.pane = pane
        if pane == .php { await model.loadAvailable() }
        let host = NSHostingView(rootView: MainWindow(model: model).frame(width: 940, height: 560))
        host.frame.size = NSSize(width: 940, height: 560)
        // AppKit-backed controls (tables, menus, toggles) only draw inside a
        // window; it never appears on screen.
        let window = NSWindow(
            contentRect: host.frame, styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        try? await Task.sleep(nanoseconds: 600_000_000)
        guard let rep = host.bitmapImageRepForCachingDisplay(in: host.bounds) else { return }
        host.cacheDisplay(in: host.bounds, to: rep)
        try? rep.representation(using: .png, properties: [:])?.write(to: url)
    }
}
