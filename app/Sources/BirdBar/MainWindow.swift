import AppKit
import SwiftUI

enum Pane: String, CaseIterable, Identifiable {
    case sites, proxies, folders, php, settings
    var id: String { rawValue }

    var title: String {
        switch self {
        case .sites: "Sites"
        case .proxies: "Proxies"
        case .folders: "Folders"
        case .php: "PHP"
        case .settings: "Settings"
        }
    }

    var icon: String {
        switch self {
        case .sites: "globe"
        case .proxies: "arrow.left.arrow.right"
        case .folders: "folder"
        case .php: "chevron.left.forwardslash.chevron.right"
        case .settings: "gearshape"
        }
    }
}

/// The main window: manage sites, proxies, parked folders, PHP versions and
/// settings. While it is open Bird behaves like a regular app (Dock icon,
/// ⌘Tab); closing it returns to menu bar only.
struct MainWindow: View {
    @ObservedObject var model: BirdModel

    private var selection: Binding<Pane?> {
        Binding(get: { model.pane }, set: { if let pane = $0 { model.pane = pane } })
    }

    var body: some View {
        NavigationSplitView {
            List(Pane.allCases, selection: selection) { pane in
                Label(pane.title, systemImage: pane.icon).tag(pane)
            }
            .navigationSplitViewColumnWidth(min: 160, ideal: 180)
            .safeAreaInset(edge: .bottom) {
                HStack(spacing: 6) {
                    Circle()
                        .fill(model.state?.running == true ? Color.green : Color.red)
                        .frame(width: 7, height: 7)
                    Text(model.state?.running == true ? "Running" : "Stopped")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                }
                .padding(10)
            }
        } detail: {
            VStack(spacing: 0) {
                if let state = model.state {
                    switch model.pane {
                    case .sites: SitesPane(model: model, state: state)
                    case .proxies: ProxiesPane(model: model, state: state)
                    case .folders: FoldersPane(model: model, state: state)
                    case .php: PhpPane(model: model, state: state)
                    case .settings: SettingsPane(model: model, state: state)
                    }
                } else {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                if let error = model.error {
                    Divider()
                    HStack(alignment: .top) {
                        ErrorText(error: error)
                        Spacer()
                        Button {
                            model.error = nil
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                        }
                        .buttonStyle(.borderless)
                    }
                    .padding(10)
                    .background(Color.red.opacity(0.08))
                }
            }
            .toolbar {
                ToolbarItem(placement: .status) {
                    if let activity = model.activity {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.small)
                            Text(activity).font(.callout).foregroundStyle(.secondary)
                        }
                    } else if model.busy {
                        ProgressView().controlSize(.small)
                    }
                }
            }
        }
        .frame(minWidth: 760, minHeight: 440)
        .task { await model.refresh() }
        .onAppear {
            NSApp.setActivationPolicy(.regular)
            NSApp.activate(ignoringOtherApps: true)
        }
        .onDisappear {
            NSApp.setActivationPolicy(.accessory)
        }
    }
}

// MARK: - Sites

final class SitesUI: ObservableObject {
    @Published var query = ""
    @Published var selection = Set<Site.ID>()
    @Published var sortOrder = [KeyPathComparator(\Site.domain)]
    @Published var unlinkCandidate: Site?
}

struct SitesPane: View {
    @ObservedObject var model: BirdModel
    let state: BirdState
    @StateObject private var ui = SitesUI()

    private var rows: [Site] {
        let q = ui.query.trimmingCharacters(in: .whitespaces).lowercased()
        let matching = q.isEmpty
            ? state.sites
            : state.sites.filter { $0.domain.contains(q) || $0.path.lowercased().contains(q) }
        return matching.sorted(using: ui.sortOrder)
    }

    var body: some View {
        Table(rows, selection: $ui.selection, sortOrder: $ui.sortOrder) {
            TableColumn("Site", value: \.domain) { site in
                VStack(alignment: .leading, spacing: 1) {
                    Text(site.domain).fontWeight(.medium)
                    Text(abbreviate(site.path))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                .padding(.vertical, 2)
            }
            TableColumn("PHP") { site in
                SitePhpMenu(site: site, state: state, model: model)
            }
            .width(min: 130, ideal: 150)
            TableColumn("HTTPS") { site in
                Toggle("Redirect HTTP to HTTPS", isOn: Binding(
                    get: { site.secure },
                    set: { model.perform($0 ? "secure" : "unsecure", site.name) }
                ))
                .labelsHidden()
                .help("Redirect HTTP to HTTPS (HTTPS always works)")
            }
            .width(52)
            TableColumn("Type", value: \.kind) { site in
                Text(site.kind == "linked" ? "Linked" : "Parked").foregroundStyle(.secondary)
            }
            .width(64)
            TableColumn("") { site in
                HStack(spacing: 10) {
                    Button { model.open(site.url) } label: { Image(systemName: "safari") }
                        .help("Open \(site.url)")
                    Button { model.reveal(site.path) } label: { Image(systemName: "folder") }
                        .help("Show in Finder")
                    Button { model.openInTerminal(site.path) } label: { Image(systemName: "terminal") }
                        .help("Open in Terminal")
                }
                .buttonStyle(.borderless)
            }
            .width(90)
        }
        .contextMenu(forSelectionType: Site.ID.self) { ids in
            if let site = site(ids.first) {
                Button("Open in Browser") { model.open(site.url) }
                Button("Show in Finder") { model.reveal(site.path) }
                Button("Open in Terminal") { model.openInTerminal(site.path) }
                Button("Copy URL") { model.copy(site.url) }
                Divider()
                Button(site.secure ? "Stop Redirecting to HTTPS" : "Redirect HTTP to HTTPS") {
                    model.perform(site.secure ? "unsecure" : "secure", site.name)
                }
                if site.kind == "linked" {
                    Divider()
                    Button("Unlink…") { ui.unlinkCandidate = site }
                }
            }
        } primaryAction: { ids in
            ids.compactMap { site($0) }.forEach { model.open($0.url) }
        }
        .searchable(text: $ui.query, placement: .toolbar, prompt: "Filter sites")
        .navigationTitle("Sites")
        .navigationSubtitle("\(state.sites.count) sites · .\(state.tld)")
        .toolbar {
            ToolbarItem {
                Button {
                    linkFolder()
                } label: {
                    Label("Link Folder…", systemImage: "link.badge.plus")
                }
                .help("Serve a project folder as <name>.\(state.tld)")
            }
            ToolbarItem {
                Button {
                    Task { await model.refresh() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
            }
        }
        .confirmationDialog(
            "Unlink \(ui.unlinkCandidate?.domain ?? "")?",
            isPresented: Binding(
                get: { ui.unlinkCandidate != nil },
                set: { if !$0 { ui.unlinkCandidate = nil } }
            ),
            presenting: ui.unlinkCandidate
        ) { site in
            Button("Unlink", role: .destructive) { model.perform("unlink", site.name) }
        } message: { site in
            Text("Bird stops serving \(site.domain). The folder \(abbreviate(site.path)) is not touched.")
        }
        .overlay {
            if state.sites.isEmpty {
                VStack(spacing: 8) {
                    Text("No sites yet").font(.title3)
                    Text("Link a project folder, or park a folder of projects under Folders.")
                        .foregroundStyle(.secondary)
                    Button("Link Folder…") { linkFolder() }
                }
            }
        }
    }

    private func site(_ id: Site.ID?) -> Site? {
        state.sites.first { $0.id == id }
    }

    private func linkFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.prompt = "Link"
        panel.message = "Choose a project folder. It is served as <folder name>.\(state.tld)."
        if panel.runModal() == .OK, let url = panel.url {
            model.perform(["link"], cwd: url)
        }
    }
}

/// PHP version of a site: the default (global or .php-version) or a pin.
struct SitePhpMenu: View {
    let site: Site
    let state: BirdState
    @ObservedObject var model: BirdModel

    var body: some View {
        Menu {
            Button {
                model.perform("unisolate", "--site", site.name)
            } label: {
                checked(site.phpSource != "isolated", defaultLabel)
            }
            Divider()
            ForEach(state.phpVersions) { v in
                Button {
                    model.perform("isolate", v.minor, "--site", site.name)
                } label: {
                    checked(site.phpSource == "isolated" && site.php == v.minor, "Pin to \(v.version)")
                }
            }
        } label: {
            Text(currentLabel).foregroundStyle(site.phpInstalled ? Color.primary : Color.red)
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help(site.phpInstalled ? help : "PHP \(site.php ?? "") is not installed")
    }

    private var defaultLabel: String {
        if site.phpSource == "php-version-file", let php = site.php {
            return "Default (.php-version: \(php))"
        }
        return "Default (global \(state.php ?? "–"))"
    }

    private var currentLabel: String {
        let php = site.php ?? "–"
        switch site.phpSource {
        case "isolated": return "\(php) · pinned"
        case "php-version-file": return "\(php) · .php-version"
        default: return php
        }
    }

    private var help: String {
        switch site.phpSource {
        case "isolated": return "Pinned with bird isolate"
        case "php-version-file": return "From the project's .php-version file"
        default: return "Global PHP version"
        }
    }
}

@ViewBuilder
func checked(_ on: Bool, _ title: String) -> some View {
    if on {
        Label(title, systemImage: "checkmark")
    } else {
        Text(title)
    }
}

func abbreviate(_ path: String) -> String {
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    return path.hasPrefix(home) ? "~" + path.dropFirst(home.count) : path
}

// MARK: - Proxies

final class ProxiesUI: ObservableObject {
    @Published var adding = false
    @Published var name = ""
    @Published var upstream = "http://127.0.0.1:"
}

struct ProxiesPane: View {
    @ObservedObject var model: BirdModel
    let state: BirdState
    @StateObject private var ui = ProxiesUI()

    var body: some View {
        Table(state.proxies) {
            TableColumn("Domain") { proxy in
                Button(proxy.domain) { model.open(proxy.url) }
                    .buttonStyle(.link)
            }
            TableColumn("Upstream") { proxy in
                Text(proxy.upstream).foregroundStyle(.secondary).textSelection(.enabled)
            }
            TableColumn("HTTPS") { proxy in
                Toggle("Redirect HTTP to HTTPS", isOn: Binding(
                    get: { proxy.secure },
                    set: { model.perform($0 ? "secure" : "unsecure", proxy.name) }
                ))
                .labelsHidden()
            }
            .width(52)
            TableColumn("") { proxy in
                Button(role: .destructive) {
                    model.perform("unproxy", proxy.name)
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .help("Remove \(proxy.domain)")
            }
            .width(30)
        }
        .navigationTitle("Proxies")
        .navigationSubtitle("Serve a .\(state.tld) domain from another server, e.g. Vite or Docker")
        .toolbar {
            ToolbarItem {
                Button {
                    ui.adding = true
                } label: {
                    Label("Add Proxy…", systemImage: "plus")
                }
            }
        }
        .overlay {
            if state.proxies.isEmpty {
                VStack(spacing: 8) {
                    Text("No proxies").font(.title3)
                    Text("Send e.g. vite.\(state.tld) to http://127.0.0.1:5173, with HTTPS and WebSockets.")
                        .foregroundStyle(.secondary)
                    Button("Add Proxy…") { ui.adding = true }
                }
            }
        }
        .sheet(isPresented: $ui.adding) {
            VStack(alignment: .leading, spacing: 12) {
                Text("Add Proxy").font(.headline)
                Form {
                    TextField("Name", text: $ui.name, prompt: Text("vite"))
                    TextField("Upstream", text: $ui.upstream, prompt: Text("http://127.0.0.1:5173"))
                }
                Text(ui.name.isEmpty ? " " : "https://\(ui.name).\(state.tld) → \(ui.upstream)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                HStack {
                    Spacer()
                    Button("Cancel") { ui.adding = false }
                        .keyboardShortcut(.cancelAction)
                    Button("Add") {
                        model.perform("proxy", ui.name, ui.upstream)
                        ui.adding = false
                        ui.name = ""
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(ui.name.isEmpty || ui.upstream.isEmpty)
                }
            }
            .padding(20)
            .frame(width: 420)
        }
    }
}

// MARK: - Parked folders

struct FoldersPane: View {
    @ObservedObject var model: BirdModel
    let state: BirdState

    var body: some View {
        Table(state.paths.map(ParkedFolder.init)) {
            TableColumn("Folder") { folder in
                Text(abbreviate(folder.path))
            }
            TableColumn("Sites") { folder in
                Text("\(count(in: folder.path))").foregroundStyle(.secondary)
            }
            .width(50)
            TableColumn("") { folder in
                HStack(spacing: 10) {
                    Button { model.reveal(folder.path) } label: { Image(systemName: "folder") }
                        .help("Show in Finder")
                    Button("Forget") { model.perform("forget", folder.path) }
                        .help("Stop serving the folders in here (nothing is deleted)")
                }
                .buttonStyle(.borderless)
            }
            .width(110)
        }
        .navigationTitle("Folders")
        .navigationSubtitle("Every folder inside a parked folder is served as <folder>.\(state.tld)")
        .toolbar {
            ToolbarItem {
                Button {
                    park()
                } label: {
                    Label("Park Folder…", systemImage: "plus")
                }
            }
        }
        .overlay {
            if state.paths.isEmpty {
                VStack(spacing: 8) {
                    Text("No parked folders").font(.title3)
                    Text("Park e.g. ~/Code and every project in it becomes <project>.\(state.tld).")
                        .foregroundStyle(.secondary)
                    Button("Park Folder…") { park() }
                }
            }
        }
    }

    private func count(in folder: String) -> Int {
        state.sites.filter {
            $0.kind == "parked" && ($0.path as NSString).deletingLastPathComponent == folder
        }.count
    }

    private func park() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.prompt = "Park"
        panel.message = "Choose a folder of projects. Each project in it is served as <project>.\(state.tld)."
        if panel.runModal() == .OK, let url = panel.url {
            model.perform("park", url.path)
        }
    }
}

struct ParkedFolder: Identifiable {
    var id: String { path }
    let path: String
}

// MARK: - PHP versions

struct PhpRow: Identifiable {
    var id: String { minor }
    let minor: String
    let installed: PhpVersion?
    let latest: String?
}

final class PhpUI: ObservableObject {
    @Published var removeCandidate: String?
}

struct PhpPane: View {
    @ObservedObject var model: BirdModel
    let state: BirdState
    @StateObject private var ui = PhpUI()

    private var rows: [PhpRow] {
        var minors = Set(state.phpVersions.map(\.minor))
        for a in model.available ?? [] { minors.insert(a.minor) }
        return minors
            .sorted { $0.compare($1, options: .numeric) == .orderedDescending }
            .map { minor in
                PhpRow(
                    minor: minor,
                    installed: state.phpVersions.first { $0.minor == minor },
                    latest: model.available?.first { $0.minor == minor }?.version)
            }
    }

    var body: some View {
        Table(rows) {
            TableColumn("Version") { row in
                HStack(spacing: 6) {
                    Text("PHP \(row.minor)").fontWeight(.semibold)
                    if state.php == row.minor {
                        Text("Global")
                            .font(.caption.weight(.medium))
                            .padding(.horizontal, 6)
                            .padding(.vertical, 1)
                            .background(Capsule().fill(Color.accentColor.opacity(0.15)))
                            .foregroundStyle(Color.accentColor)
                    }
                }
            }
            .width(min: 110, ideal: 130)
            TableColumn("Status") { row in
                PhpStatus(row: row, update: model.updates[row.minor], loading: model.available == nil)
            }
            .width(min: 180, ideal: 220)
            TableColumn("") { row in
                HStack(spacing: 8) {
                    if let installed = row.installed {
                        if let update = model.updates[row.minor] {
                            Button("Update to \(update)") {
                                model.perform(
                                    ["php:update", row.minor], activity: "Updating PHP \(row.minor)…",
                                    then: { await model.loadAvailable() })
                            }
                            .buttonStyle(.borderedProminent)
                        }
                        if state.php != row.minor {
                            Button("Make Global") { model.perform("use", row.minor) }
                                .buttonStyle(.bordered)
                        }
                        Button(role: .destructive) {
                            ui.removeCandidate = installed.minor
                        } label: {
                            Text("Remove…")
                        }
                        .buttonStyle(.bordered)
                    } else {
                        Button("Install") {
                            model.perform(
                                ["php:install", row.minor], activity: "Installing PHP \(row.minor)…",
                                then: { await model.loadAvailable() })
                        }
                        .buttonStyle(.borderedProminent)
                    }
                }
                .controlSize(.small)
                .disabled(model.activity != nil)
            }
            .width(min: 260, ideal: 300)
        }
        .navigationTitle("PHP")
        .navigationSubtitle(
            "Opcache \(state.opcache ? "on" : "off") · Xdebug \(state.xdebug == "auto" ? "on trigger" : state.xdebug)")
        .toolbar {
            ToolbarItem {
                if !model.updates.isEmpty {
                    Button {
                        model.perform(
                            ["php:update"], activity: "Updating PHP…",
                            then: { await model.loadAvailable() })
                    } label: {
                        Label("Update All", systemImage: "arrow.down.circle")
                    }
                    .disabled(model.activity != nil)
                }
            }
            ToolbarItem {
                Button {
                    Task { await model.loadAvailable() }
                } label: {
                    Label("Check for Updates", systemImage: "arrow.clockwise")
                }
            }
        }
        .task { await model.loadAvailable() }
        .confirmationDialog(
            "Remove PHP \(ui.removeCandidate ?? "")?",
            isPresented: Binding(
                get: { ui.removeCandidate != nil },
                set: { if !$0 { ui.removeCandidate = nil } }
            ),
            presenting: ui.removeCandidate
        ) { minor in
            Button("Remove", role: .destructive) { model.perform("php:remove", minor) }
        } message: { minor in
            let users = state.sites.filter { $0.php == minor }.map(\.domain)
            Text(users.isEmpty
                ? "Its php.ini and conf.d are deleted too."
                : "Used by \(users.joined(separator: ", ")). Its php.ini and conf.d are deleted too.")
        }
    }
}

struct PhpStatus: View {
    let row: PhpRow
    let update: String?
    let loading: Bool

    var body: some View {
        if let installed = row.installed {
            HStack(spacing: 5) {
                Image(systemName: update == nil ? "checkmark.circle.fill" : "arrow.down.circle.fill")
                    .foregroundStyle(update == nil ? Color.green : Color.orange)
                Text(installed.version)
                if installed.source != "bird" {
                    Text("(\(installed.source))").foregroundStyle(.secondary)
                        .help("Installed from \(installed.source); updating installs Bird's build")
                }
                if update != nil {
                    Text("update available").foregroundStyle(.orange).font(.caption)
                }
            }
        } else {
            HStack(spacing: 5) {
                Image(systemName: "circle.dashed").foregroundStyle(.tertiary)
                Text(row.latest.map { "Not installed · \($0)" } ?? (loading ? "…" : "Not installed"))
                    .foregroundStyle(.secondary)
            }
        }
    }
}

// MARK: - Updates

struct UpdatesSection: View {
    @ObservedObject var model: BirdModel
    let state: BirdState

    var body: some View {
        LabeledContent("Version") {
            Text("Bird \(state.birdVersion)").textSelection(.enabled)
        }
        HStack(alignment: .center, spacing: 8) {
            status
            Spacer()
            if model.update?.available == true {
                if let url = model.update?.url {
                    Button("Release Notes") { model.open(url) }
                        .buttonStyle(.link)
                }
                Button(model.updating ? "Updating…" : "Update Now") { model.installUpdate() }
                    .buttonStyle(.borderedProminent)
                    .disabled(model.updating)
            }
            Button("Check for Updates") {
                Task { await model.checkForUpdates() }
            }
            .disabled(model.checkingForUpdate || model.updating)
        }
        if let error = model.updateError {
            ErrorText(error: error)
        }
        Toggle("Check for updates when Bird starts", isOn: Binding(
            get: { model.checkOnLaunch },
            set: { model.checkOnLaunch = $0 }
        ))
        .help("Only checks and tells you; updates are never installed without a click.")
    }

    @ViewBuilder
    private var status: some View {
        if model.updating {
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text("Updating… Bird restarts when it is done.").foregroundStyle(.secondary)
            }
        } else if model.checkingForUpdate {
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text("Checking for updates…").foregroundStyle(.secondary)
            }
        } else if let update = model.update {
            if update.available, let latest = update.latest {
                Label("Bird \(latest) is available", systemImage: "arrow.down.circle.fill")
                    .foregroundStyle(.orange)
            } else {
                VStack(alignment: .leading, spacing: 1) {
                    Label("Bird is up to date", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                    if let checked = model.lastUpdateCheck {
                        Text("Checked \(checked.formatted(date: .omitted, time: .shortened))")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }
        } else {
            Text("Not checked yet").foregroundStyle(.secondary)
        }
    }
}

// MARK: - Settings

struct SettingsPane: View {
    @ObservedObject var model: BirdModel
    let state: BirdState

    var body: some View {
        Form {
            Section("PHP") {
                QuickSettings(model: model, state: state)
            }
            Section("Updates") {
                UpdatesSection(model: model, state: state)
            }
            Section("Bird") {
                LabeledContent("Status", value: statusLine(state))
                LabeledContent("Domains", value: "*.\(state.tld)")
                LabeledContent("Home") {
                    Button(abbreviate(state.birdHome)) { model.reveal(state.birdHome) }
                        .buttonStyle(.link)
                }
                LabeledContent("Log") {
                    Button("Open in Console") { model.openLog() }
                        .buttonStyle(.link)
                }
                Toggle("Open menu bar app at login", isOn: Binding(
                    get: { model.launchAtLogin },
                    set: { model.launchAtLogin = $0 }
                ))
            }
            Section("Doctor") {
                HStack {
                    Text("Checks PATH, DNS, ports, the certificate authority and PHP.")
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("Run Doctor") { model.runDoctor() }
                }
                if let output = model.doctorOutput {
                    Text(output)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Settings")
    }
}
