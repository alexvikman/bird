import AppKit
import SwiftUI

@main
struct BirdBarApp: App {
    @StateObject private var model = BirdModel()

    init() {
        // `Bird --snapshot-window sites window.png`: render a pane of the main
        // window to an image and exit (for checking layouts without clicking).
        let args = CommandLine.arguments
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
            // Filled while Bird runs, outlined when it is stopped.
            Image(systemName: model.state?.running == true ? "bird.fill" : "bird")
        }
        .menuBarExtraStyle(.menu)

        Window("Bird", id: "main") {
            MainWindow(model: model)
        }
        .defaultSize(width: 940, height: 600)
    }
}

@MainActor
enum Snapshot {
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
