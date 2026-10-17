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
    /// Bird's swift (as in the app icon): filled while Bird runs, outlined
    /// when it is stopped, with a small dot in the top right corner when an
    /// update is waiting. A template image, so it follows the menu bar's
    /// light/dark appearance.
    static func image(running: Bool, dot: Bool) -> NSImage {
        let bird = NSRect(x: 0, y: 0, width: 20, height: 17)
        let size = NSSize(width: bird.width + (dot ? 8 : 0), height: bird.height)
        let image = NSImage(size: size, flipped: false) { rect in
            // An outline is inset so its stroke isn't clipped at the edges.
            let path = swift(in: running ? bird : bird.insetBy(dx: 0.6, dy: 0.6))
            NSColor.black.set()
            if running {
                path.fill()
            } else {
                path.lineWidth = 1.1
                path.lineJoinStyle = .round
                path.stroke()
            }
            if dot {
                let d: CGFloat = 5.5
                NSBezierPath(ovalIn: NSRect(x: rect.width - d, y: rect.height - d, width: d, height: d)).fill()
            }
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = dot ? "Bird, update available" : "Bird"
        return image
    }

    /// A common swift seen from below (long sickle wings, short forked
    /// tail), drawn in a 100×100 box and fitted into `rect`.
    static func swift(in rect: NSRect) -> NSBezierPath {
        let p = NSBezierPath()
        func pt(_ x: CGFloat, _ y: CGFloat) -> NSPoint { NSPoint(x: x, y: y) }
        p.move(to: pt(24.6, 77.2))                                              // beak
        p.curve(to: pt(31, 79), controlPoint1: pt(26, 79), controlPoint2: pt(28.5, 79.5))
        p.curve(to: pt(39, 75.5), controlPoint1: pt(34, 78.5), controlPoint2: pt(37, 77))
        p.curve(to: pt(97.6, 88.9), controlPoint1: pt(54, 85), controlPoint2: pt(81, 92))   // right wing
        p.curve(to: pt(50.7, 63.5), controlPoint1: pt(81, 82.5), controlPoint2: pt(57, 74))
        p.curve(to: pt(55, 50), controlPoint1: pt(53, 59), controlPoint2: pt(55, 54))
        p.curve(to: pt(69, 39), controlPoint1: pt(61, 47), controlPoint2: pt(66, 43))       // tail
        p.curve(to: pt(57.5, 43.5), controlPoint1: pt(64, 40), controlPoint2: pt(60, 41.5))
        p.curve(to: pt(60.5, 31.5), controlPoint1: pt(59, 39), controlPoint2: pt(60.5, 35))
        p.curve(to: pt(46, 52), controlPoint1: pt(54, 38), controlPoint2: pt(49, 46))
        p.curve(to: pt(3.4, 10.5), controlPoint1: pt(30, 46), controlPoint2: pt(12, 26))    // left wing
        p.curve(to: pt(27, 66.2), controlPoint1: pt(6, 30), controlPoint2: pt(18, 52))
        p.curve(to: pt(24.6, 77.2), controlPoint1: pt(25, 70), controlPoint2: pt(23.5, 74)) // throat
        p.close()
        // The drawing spans x 3…98, y 10…89; scale it into rect, keeping its shape.
        let box = NSRect(x: 3, y: 10, width: 95, height: 79)
        let scale = min(rect.width / box.width, rect.height / box.height)
        var t = AffineTransform()
        t.translate(x: rect.midX - box.midX * scale, y: rect.midY - box.midY * scale)
        t.scale(scale)
        p.transform(using: t)
        return p
    }
}

@MainActor
enum Snapshot {
    /// The menu bar icons (running, running with an update, stopped) at 4×,
    /// on a light and a dark menu bar.
    static func icons(to url: URL) {
        let icons = [
            MenuBarIcon.image(running: true, dot: false),
            MenuBarIcon.image(running: true, dot: true),
            MenuBarIcon.image(running: false, dot: false),
        ]
        let zoom: CGFloat = 4
        let image = NSImage(size: NSSize(width: 480, height: 200), flipped: false) { _ in
            for (row, (bg, fg)) in [(NSColor.white, NSColor.black), (NSColor.black, NSColor.white)].enumerated() {
                bg.setFill()
                NSRect(x: 0, y: CGFloat(row) * 100, width: 480, height: 100).fill()
                for (col, icon) in icons.enumerated() {
                    let size = NSSize(width: icon.size.width * zoom, height: icon.size.height * zoom)
                    let tinted = NSImage(size: size, flipped: false) { r in
                        icon.draw(in: r)
                        fg.set()
                        r.fill(using: .sourceAtop)
                        return true
                    }
                    tinted.draw(at: NSPoint(x: 20 + CGFloat(col) * 150, y: CGFloat(row) * 100 + 16),
                                from: .zero, operation: .sourceOver, fraction: 1)
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
        let alert = model.makeUpdateAlert() ?? model.upToDateAlert()
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
