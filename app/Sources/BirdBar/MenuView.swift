import SwiftUI

// View-local state lives in small ObservableObjects instead of @State: with
// only the command line tools (no Xcode), SwiftUI's @State macro plugin is
// not available.

/// The menu bar menu: a native macOS menu (closes on every choice) for the
/// things you do often. Sites are managed in the main window.
struct MenuContent: View {
    @ObservedObject var model: BirdModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        let state = model.state

        Text(statusLine(state))
        if model.error != nil {
            Button("⚠︎ Show Last Error…") { show(model.pane) }
        }
        if let update = model.update, update.available, let latest = update.latest {
            Button("Update to Bird \(latest)…") { show(.settings) }
        }

        Divider()

        Button("Open Bird…") { show(.sites) }
            .keyboardShortcut("o")
        if let state, !state.sites.isEmpty || !state.proxies.isEmpty {
            Menu("Sites") {
                ForEach(state.sites) { site in
                    Button(site.domain) { model.open(site.url) }
                }
                if !state.proxies.isEmpty {
                    Divider()
                    ForEach(state.proxies) { proxy in
                        Button(proxy.domain) { model.open(proxy.url) }
                    }
                }
            }
        }

        Divider()

        if state?.running == true {
            Button("Restart Bird") { model.perform("restart", "--daemon") }
                .keyboardShortcut("r")
            Button("Restart PHP") { model.perform("restart") }
            Button("Stop Bird…") { model.stopService() }
        } else if state?.serviceInstalled == true {
            Button("Start Bird…") { model.startService() }
                .keyboardShortcut("r")
        } else if state != nil {
            Text("Run `bird install` to set Bird up")
        }

        if let state, !state.phpVersions.isEmpty {
            Divider()
            // Newest first, ⌘1 … ⌘9 like Herd.
            let versions = Array(state.phpVersions.reversed())
            ForEach(Array(versions.enumerated()), id: \.element.id) { index, v in
                let toggle = Toggle("Use PHP \(v.minor)", isOn: Binding(
                    get: { state.php == v.minor },
                    set: { if $0 { model.perform("use", v.minor) } }
                ))
                if index < 9 {
                    toggle.keyboardShortcut(KeyEquivalent(Character(String(index + 1))))
                } else {
                    toggle
                }
            }

            Divider()

            Toggle("Opcache", isOn: Binding(
                get: { state.opcache },
                set: { model.perform("opcache", $0 ? "on" : "off") }
            ))
            Picker("Xdebug", selection: Binding(
                get: { state.xdebug },
                set: { model.perform("xdebug", $0) }
            )) {
                Text("Off").tag("off")
                Text("On Trigger").tag("auto")
                Text("Always").tag("on")
            }
            .pickerStyle(.menu)
        }

        Divider()

        Button("Settings…") { show(.settings) }
            .keyboardShortcut(",")
        Button("Check for Updates…") {
            show(.settings)
            Task { await model.checkForUpdates() }
        }
            .keyboardShortcut("u")
        Button("Open Log") { model.openLog() }

        Divider()

        Button("Quit") { NSApplication.shared.terminate(nil) }
            .keyboardShortcut("q")
    }

    private func show(_ pane: Pane) {
        model.pane = pane
        openWindow(id: "main")
        NSApp.activate(ignoringOtherApps: true)
    }
}

/// "Running · PHP 8.5 · .test · FPM 8.5, 8.5 Xdebug"
func statusLine(_ state: BirdState?) -> String {
    guard let state else { return "Bird" }
    guard let daemon = state.daemon else { return "Bird is stopped" }
    var parts = ["Bird is running"]
    if let php = state.php { parts.append("PHP \(php)") }
    parts.append(".\(state.tld)")
    let pools = (daemon.fpm ?? []).map { pool in
        pool.version.hasSuffix("-xdebug")
            ? pool.version.replacingOccurrences(of: "-xdebug", with: " Xdebug")
            : pool.version
    }
    if !pools.isEmpty {
        parts.append("FPM " + pools.joined(separator: ", "))
    }
    return parts.joined(separator: " · ")
}

/// Global PHP version, opcache and Xdebug for the settings pane.
struct QuickSettings: View {
    @ObservedObject var model: BirdModel
    let state: BirdState

    var body: some View {
        Picker("PHP", selection: Binding(
            get: { state.php ?? "" },
            set: { model.perform("use", $0) }
        )) {
            ForEach(state.phpVersions) { v in
                Text("PHP \(v.version)").tag(v.minor)
            }
        }
        Toggle("Opcache", isOn: Binding(
            get: { state.opcache },
            set: { model.perform("opcache", $0 ? "on" : "off") }
        ))
        Picker("Xdebug", selection: Binding(
            get: { state.xdebug },
            set: { model.perform("xdebug", $0) }
        )) {
            Text("Off").tag("off")
            Text("On trigger").tag("auto")
            Text("Always").tag("on")
        }
        .pickerStyle(.segmented)
        .help("On trigger: only requests from the Xdebug browser extension (or ?XDEBUG_SESSION=1) run with Xdebug.")
    }
}

struct ErrorText: View {
    let error: String

    var body: some View {
        Text(error)
            .font(.caption)
            .foregroundStyle(.red)
            .textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
    }
}
