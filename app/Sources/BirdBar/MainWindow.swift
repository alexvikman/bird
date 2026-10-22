import AppKit
import SwiftUI

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
                HStack(spacing: 10) {
                    PaneIcon(pane: pane)
                    Text(pane.title)
                }
                .padding(.vertical, 3)
                .tag(pane)
            }
            .withoutSidebarToggle()
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
                    case .about: AboutPane(model: model, state: state)
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
            // White behind the content (like System Settings), not the window's
            // grey, also under the toolbar so the two meet without a seam.
            .background(Color(nsColor: .textBackgroundColor).ignoresSafeArea())
            // No toolbar material of its own (it is grey while the window has
            // focus): the white shows through, and the sidebar runs to the top.
            .toolbarBackground(.hidden, for: .windowToolbar)
            .toolbar {
                // Only while something runs: an empty status item still
                // takes the middle of the toolbar, where the window is dragged.
                if model.activity != nil || model.busy {
                    ToolbarItem(placement: .status) {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.small)
                            if let activity = model.activity {
                                Text(activity).font(.callout).foregroundStyle(.secondary)
                            }
                        }
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
        PaneScaffold(
            pane: .sites, tld: state.tld, layout: .table,
            subtitle: "\(state.sites.count) sites · .\(state.tld)",
            actions: [
                PaneAction(
                    title: "Link Folder…", icon: "link.badge.plus",
                    help: "Serve a project folder as <name>.\(state.tld)", run: linkFolder),
                PaneAction(
                    title: "Refresh", icon: "arrow.clockwise", help: "Reload the list of sites",
                    run: { Task { await model.refresh() } }),
            ]
        ) {
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
        PaneScaffold(
            pane: .proxies, tld: state.tld, layout: .table,
            actions: [
                PaneAction(
                    title: "Add Proxy…", icon: "plus",
                    help: "Serve a .\(state.tld) domain from another server", run: { ui.adding = true }),
            ]
        ) {
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
}

// MARK: - Parked folders

struct FoldersPane: View {
    @ObservedObject var model: BirdModel
    let state: BirdState

    var body: some View {
        PaneScaffold(
            pane: .folders, tld: state.tld, layout: .table,
            actions: [
                PaneAction(
                    title: "Park Folder…", icon: "plus",
                    help: "Serve every project in a folder as <project>.\(state.tld)", run: park),
            ]
        ) {
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
    @Published var showUploadPerVersion = false
    @Published var showMemoryPerVersion = false
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

    private var actions: [PaneAction] {
        var actions: [PaneAction] = []
        if !model.updates.isEmpty {
            actions.append(PaneAction(
                title: "Update All", icon: "arrow.down.circle",
                help: "Install every PHP update listed below", disabled: model.activity != nil
            ) {
                model.perform(["php:update"], activity: "Updating PHP…", then: { await model.loadAvailable() })
            })
        }
        actions.append(PaneAction(
            title: "Check for Updates", icon: "arrow.clockwise",
            help: "Check for new PHP versions and builds"
        ) {
            Task { await model.loadAvailable() }
        })
        return actions
    }

    var body: some View {
        PaneScaffold(pane: .php, tld: state.tld, actions: actions) {
            Card(title: "Versions") {
                ForEach(Array(rows.enumerated()), id: \.element.id) { i, row in
                    if i > 0 { CardDivider() }
                    PhpVersionRow(model: model, state: state, ui: ui, row: row)
                }
            }
            Card(footer: "Empty fields use the value in each version's php.ini.") {
                PhpLimits(model: model, state: state, ui: ui)
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

/// One PHP version: `8.5 (8.5.11)`, its status, and what can be done with it.
/// An update names the version it installs.
struct PhpVersionRow: View {
    @ObservedObject var model: BirdModel
    let state: BirdState
    @ObservedObject var ui: PhpUI
    let row: PhpRow

    var body: some View {
        HStack(spacing: 12) {
            HStack(spacing: 6) {
                Text(row.minor).fontWeight(.semibold)
                if let installed = row.installed {
                    Text("(\(installed.version))").foregroundStyle(.secondary)
                }
                if state.php == row.minor {
                    Text("Global")
                        .font(.caption.weight(.medium))
                        .padding(.horizontal, 6)
                        .padding(.vertical, 1)
                        .background(Capsule().fill(Color.accentColor.opacity(0.15)))
                        .foregroundStyle(Color.accentColor)
                        .help("The PHP version sites and the php command use unless a site pins another")
                }
            }
            .frame(width: 170, alignment: .leading)
            PhpStatus(row: row, update: model.updates[row.minor], loading: model.available == nil)
            Spacer()
            HStack(spacing: 8) {
                if let installed = row.installed {
                    if let update = model.updates[row.minor] {
                        Button(update == installed.version ? "Update Build" : "Update to \(update)") {
                            model.perform(
                                ["php:update", row.minor], activity: "Updating PHP \(row.minor)…",
                                then: { await model.loadAvailable() })
                        }
                        .buttonStyle(.borderedProminent)
                        .help(update == installed.version
                            ? "Bird has rebuilt PHP \(installed.version), e.g. with fixes or other extensions"
                            : "Install PHP \(update)")
                    }
                    if state.php != row.minor {
                        Button("Make Global") { model.perform("use", row.minor) }
                    }
                    Button("Remove…", role: .destructive) { ui.removeCandidate = installed.minor }
                } else {
                    Button(row.latest.map { "Install \($0)" } ?? "Install") {
                        model.perform(
                            ["php:install", row.minor], activity: "Installing PHP \(row.minor)…",
                            then: { await model.loadAvailable() })
                    }
                }
            }
            .controlSize(.small)
            .disabled(model.activity != nil)
        }
        .cardRow()
    }
}

struct PhpStatus: View {
    let row: PhpRow
    let update: String?
    let loading: Bool

    var body: some View {
        if let installed = row.installed {
            HStack(spacing: 5) {
                if let update {
                    // What the update brings: a newer version, or a new build
                    // of the same one (other extensions or fixes in Bird's build).
                    Text(update == installed.version ? "New build available" : "\(update) available")
                        .foregroundStyle(.orange)
                } else {
                    Text("Installed").foregroundStyle(.green)
                }
                if installed.source != "bird" {
                    Text("(\(installed.source))").foregroundStyle(.secondary)
                        .help("Installed from \(installed.source); updating installs Bird's build")
                }
            }
            .fontWeight(.medium)
        } else {
            Text(loading ? "…" : "Not installed").foregroundStyle(.secondary)
        }
    }
}

/// Upload size and memory limit (`bird php:limits`), like Herd's: one value
/// for all versions, and under a disclosure one per version.
struct PhpLimits: View {
    @ObservedObject var model: BirdModel
    let state: BirdState
    @ObservedObject var ui: PhpUI

    /// The global version's own value, shown when Bird sets none.
    private var global: PhpVersion? {
        state.phpVersions.first { $0.minor == state.php } ?? state.phpVersions.last
    }

    var body: some View {
        LimitGroup(
            title: "Max File Upload Size",
            caption: "The largest file PHP accepts as an upload, in MB. Form posts may be as large.",
            expanded: $ui.showUploadPerVersion,
            value: state.phpUploadMb,
            placeholder: global?.uploadMb,
            set: { set("--upload", $0) }
        ) {
            ForEach(state.phpVersions.reversed()) { v in
                CardDivider()
                LimitRow(
                    title: v.minor, caption: nil, value: v.uploadMbOverride,
                    placeholder: state.phpUploadMb ?? v.uploadMb,
                    set: { set("--upload", $0, php: v.minor) }
                )
                .padding(.leading, 26)
            }
        }
        CardDivider()
        LimitGroup(
            title: "Memory Limit",
            caption: "The most memory a PHP script may use, in MB; -1 for no limit.",
            expanded: $ui.showMemoryPerVersion,
            value: state.phpMemoryMb,
            placeholder: global?.memoryMb,
            set: { set("--memory", $0) }
        ) {
            ForEach(state.phpVersions.reversed()) { v in
                CardDivider()
                LimitRow(
                    title: v.minor, caption: nil, value: v.memoryMbOverride,
                    placeholder: state.phpMemoryMb ?? v.memoryMb,
                    set: { set("--memory", $0, php: v.minor) }
                )
                .padding(.leading, 26)
            }
        }
    }

    private func set(_ flag: String, _ mb: Int?, php: String? = nil) {
        var args = ["php:limits", flag, mb.map(String.init) ?? "default"]
        if let php { args += ["--php", php] }
        model.perform(args)
    }
}

/// A setting for all PHP versions that opens (the chevron) to one per version.
struct LimitGroup<PerVersion: View>: View {
    let title: String
    let caption: String
    @Binding var expanded: Bool
    let value: Int?
    let placeholder: Int?
    let set: (Int?) -> Void
    @ViewBuilder let perVersion: PerVersion

    var body: some View {
        HStack(alignment: .center, spacing: 4) {
            Button {
                withAnimation(.easeInOut(duration: 0.15)) { expanded.toggle() }
            } label: {
                Image(systemName: "chevron.right")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(.secondary)
                    .rotationEffect(.degrees(expanded ? 90 : 0))
                    .frame(width: 16, height: 16)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(expanded ? "Hide the versions" : "Set it per PHP version")
            LimitRow(title: title, caption: caption, value: value, placeholder: placeholder, set: set)
        }
        .padding(.leading, 6)
        if expanded {
            perVersion
        }
    }
}

/// A title and caption on the left, a number field in MB on the right. An
/// empty field means "not set"; the placeholder shows what applies then.
struct LimitRow: View {
    let title: String
    let caption: String?
    let value: Int?
    let placeholder: Int?
    let set: (Int?) -> Void

    var body: some View {
        SettingRow(title: title, caption: caption) {
            TextField(
                title,
                value: Binding(get: { value }, set: { if $0 != value { set($0) } }),
                format: .number.grouping(.never),
                prompt: Text(placeholder.map { String($0) } ?? ""))
                .labelsHidden()
                .textFieldStyle(.roundedBorder)
                .multilineTextAlignment(.trailing)
                .frame(width: 90)
        }
    }
}

// MARK: - Updates

struct UpdatesSection: View {
    @ObservedObject var model: BirdModel
    let state: BirdState

    var body: some View {
        HStack(alignment: .center, spacing: 8) {
            status
            Spacer()
            if model.update?.available == true {
                if let url = model.update?.url {
                    Button("Release Notes") { model.open(url) }
                        .buttonStyle(.link)
                }
                // Through the same dialog as the menu: notes, then Install / Later / Skip.
                Button(model.updating ? "Updating…" : "Update…") { model.presentUpdate() }
                    .buttonStyle(.borderedProminent)
                    .disabled(model.updating)
            }
            Button("Check for Updates…") { model.checkForUpdatesInteractively() }
                .disabled(model.checkingForUpdate || model.updating)
        }
        .cardRow()
        if let error = model.updateError {
            CardDivider()
            ErrorText(error: error).cardRow()
        }
        CardDivider()
        ToggleRow(
            title: "Check for updates automatically",
            help: "At launch and once a day. Only checks and tells you; updates are never installed without a click.",
            isOn: Binding(get: { model.checkOnLaunch }, set: { model.checkOnLaunch = $0 }))
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

// MARK: - About

struct AboutPane: View {
    @ObservedObject var model: BirdModel
    let state: BirdState

    static let repository = "https://github.com/alexvikman/bird"

    var body: some View {
        PaneScaffold(pane: .about, tld: state.tld) {
            Text(version).foregroundStyle(.secondary).textSelection(.enabled)
                .padding(.top, 2)
            if let built = Self.buildDate {
                Text("Built \(built.formatted(date: .abbreviated, time: .shortened))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        } content: {
            Card(title: "Updates") {
                UpdatesSection(model: model, state: state)
            }
            Card(title: "Links") {
                link("GitHub", icon: "chevron.left.forwardslash.chevron.right", Self.repository)
                CardDivider()
                link("Release Notes", icon: "doc.text", "\(Self.repository)/releases")
                CardDivider()
                link("Report an Issue", icon: "exclamationmark.bubble", "\(Self.repository)/issues")
                CardDivider()
                link("License", icon: "checkmark.seal", "\(Self.repository)/blob/main/LICENSE")
            }
            Text("© 2026 Alexander Vikman. MIT License.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity)
        }
    }

    /// "Version 0.4.6"; the app's own version too if it differs from bird's.
    private var version: String {
        let app = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        if let app, app != state.birdVersion {
            return "Version \(state.birdVersion) (app \(app))"
        }
        return "Version \(state.birdVersion)"
    }

    /// When this copy of the app was built (its executable's date).
    private static var buildDate: Date? {
        guard let path = Bundle.main.executablePath else { return nil }
        return (try? FileManager.default.attributesOfItem(atPath: path))?[.modificationDate] as? Date
    }

    private func link(_ title: String, icon: String, _ url: String) -> some View {
        Button {
            model.open(url)
        } label: {
            HStack {
                Label {
                    Text(title)
                } icon: {
                    // Fixed width, so the titles line up whatever the symbol.
                    Image(systemName: icon).frame(width: 20)
                }
                Spacer()
                Image(systemName: "arrow.up.right").foregroundStyle(.secondary)
            }
            .cardRow()
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

// MARK: - Settings

struct SettingsPane: View {
    @ObservedObject var model: BirdModel
    let state: BirdState

    var body: some View {
        PaneScaffold(pane: .settings, tld: state.tld) {
            Card(title: "PHP") {
                SettingRow(title: "Global PHP version") {
                    Picker("Global PHP version", selection: Binding(
                        get: { state.php ?? "" },
                        set: { model.perform("use", $0) }
                    )) {
                        ForEach(state.phpVersions) { v in
                            Text("PHP \(v.version)").tag(v.minor)
                        }
                    }
                    .labelsHidden()
                    .fixedSize()
                }
                CardDivider()
                ToggleRow(
                    title: "Opcache",
                    caption: "Caches compiled PHP; files are still checked for changes on every request.",
                    isOn: Binding(
                        get: { state.opcache },
                        set: { model.perform("opcache", $0 ? "on" : "off") }))
                CardDivider()
                SettingRow(title: "Xdebug", caption: "On trigger: only requests from the Xdebug browser extension (or ?XDEBUG_SESSION=1).") {
                    Picker("Xdebug", selection: Binding(
                        get: { state.xdebug },
                        set: { model.perform("xdebug", $0) }
                    )) {
                        Text("Off").tag("off")
                        Text("On trigger").tag("auto")
                        Text("Always").tag("on")
                    }
                    .labelsHidden()
                    .pickerStyle(.segmented)
                    .fixedSize()
                }
            }
            Card(title: "Bird") {
                SettingRow(title: "Status") {
                    Text(statusLine(state)).foregroundStyle(.secondary)
                }
                CardDivider()
                SettingRow(title: "Domains") {
                    Text("*.\(state.tld)").foregroundStyle(.secondary)
                }
                CardDivider()
                SettingRow(title: "Home") {
                    Button(abbreviate(state.birdHome)) { model.reveal(state.birdHome) }
                        .buttonStyle(.link)
                }
                CardDivider()
                SettingRow(title: "Log") {
                    Button("Open in Console") { model.openLog() }
                        .buttonStyle(.link)
                }
                CardDivider()
                ToggleRow(
                    title: "Open menu bar app at login",
                    isOn: Binding(get: { model.launchAtLogin }, set: { model.launchAtLogin = $0 }))
            }
            Card(title: "Doctor") {
                SettingRow(title: "Checks PATH, DNS, ports, the certificate authority and PHP.") {
                    Button("Run Doctor") { model.runDoctor() }
                }
                if let output = model.doctorOutput {
                    CardDivider()
                    Text(output)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .cardRow()
                }
            }
        }
    }

}
