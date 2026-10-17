import AppKit
import Foundation
import ServiceManagement

// MARK: - `bird state` JSON

struct BirdState: Decodable {
    let birdVersion: String
    let birdHome: String
    let log: String
    let serviceInstalled: Bool
    let daemon: Daemon?
    let tld: String
    let php: String?
    let opcache: Bool
    let xdebug: String
    let phpVersions: [PhpVersion]
    /// Only with `bird state --available`.
    let phpAvailable: [AvailablePhp]?
    let phpAvailableError: String?
    let sites: [Site]
    let proxies: [Proxy]
    /// Parked folders.
    let paths: [String]

    var running: Bool { daemon != nil }
}

struct AvailablePhp: Decodable, Identifiable {
    var id: String { minor }
    let minor: String
    let version: String
}

struct Daemon: Decodable {
    let version: String?
    let pid: Int?
    let uptimeSecs: Int?
    let fpm: [Pool]?
}

struct Pool: Decodable, Identifiable {
    var id: String { version }
    /// `8.4` or `8.4-xdebug`
    let version: String
    let pid: Int?
    let idleSecs: Int?
}

struct PhpVersion: Decodable, Identifiable {
    var id: String { minor }
    let minor: String
    let version: String
    let source: String
    let xdebug: Bool
    /// Newer (or rebuilt) version on the mirror; only with `--available`.
    let update: String?
}

struct Site: Decodable, Identifiable {
    var id: String { name }
    let name: String
    let domain: String
    let url: String
    let path: String
    let kind: String
    let secure: Bool
    let php: String?
    /// isolated, php-version-file, global or none
    let phpSource: String
    let phpInstalled: Bool
}

struct Proxy: Decodable, Identifiable {
    var id: String { name }
    let name: String
    let domain: String
    let url: String
    let upstream: String
    let secure: Bool
}

/// `bird self-update --check --json`
struct UpdateInfo: Decodable {
    let current: String
    let latest: String?
    let available: Bool
    let url: String?
    /// Release notes (Markdown).
    let notes: String?
}

// MARK: - Talking to `bird`

/// All state comes from `bird state`, and every change goes through the
/// normal `bird` commands, so the app never touches Bird's files itself.
@MainActor
final class BirdModel: ObservableObject {
    @Published private(set) var state: BirdState?
    /// Downloadable PHP versions, loaded on demand (network).
    @Published private(set) var available: [AvailablePhp]?
    @Published private(set) var updates: [String: String] = [:]
    @Published var error: String?
    @Published private(set) var busy = false
    /// What a slow command is doing, e.g. "Installing PHP 8.2…".
    @Published private(set) var activity: String?
    @Published private(set) var doctorOutput: String?
    /// Pane the main window shows; the menu sets it before opening the window.
    @Published var pane: Pane = .sites

    // Updates (checked, never installed, on launch; installing is a click).
    @Published private(set) var update: UpdateInfo?
    @Published private(set) var checkingForUpdate = false
    @Published private(set) var lastUpdateCheck: Date?
    @Published private(set) var updateError: String?
    @Published private(set) var updating = false

    private static let checkOnLaunchKey = "checkForUpdatesOnLaunch"
    private static let lastCheckKey = "lastUpdateCheck"
    private static let skippedKey = "skippedUpdateVersion"
    /// Automatic checks: at launch, then whenever a day has passed.
    private static let checkInterval: TimeInterval = 24 * 60 * 60

    static let launchdLabel = "dev.bird.daemon"
    static let plist = "/Library/LaunchDaemons/dev.bird.daemon.plist"

    /// ~/.bird/bin/bird, or $BIRD_BIN for development.
    let bird: URL = {
        if let custom = ProcessInfo.processInfo.environment["BIRD_BIN"] {
            return URL(fileURLWithPath: custom)
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".bird/bin/bird")
    }()

    private var refreshing = false

    init() {
        lastUpdateCheck = UserDefaults.standard.object(forKey: Self.lastCheckKey) as? Date
        Task {
            await refresh()
            if checkOnLaunch { await checkForUpdates() }
        }
        // A menu bar app runs for weeks: look again once a day (the timer
        // ticks hourly so a sleeping Mac catches up soon after waking).
        Timer.scheduledTimer(withTimeInterval: 60 * 60, repeats: true) { [weak self] _ in
            Task { await self?.checkIfDue() }
        }
        // The menu is a native menu that can't refresh when it opens. Instead
        // of polling, refresh when Bird's directories change (see
        // watchBirdHome); this rare look is only a safety net.
        Timer.scheduledTimer(withTimeInterval: 5 * 60, repeats: true) { [weak self] _ in
            Task { await self?.refresh() }
        }
    }

    func refresh() async {
        if refreshing {
            refreshQueued = true
            return
        }
        refreshing = true
        repeat {
            refreshQueued = false
            await refreshOnce()
        } while refreshQueued
        refreshing = false
        watchBirdHome()
    }

    private var refreshQueued = false
    private var lastStateData: Data?

    private func refreshOnce() async {
        guard FileManager.default.isExecutableFile(atPath: bird.path) else {
            state = nil
            error = "Bird is not installed. Run `bird install` in Terminal."
            return
        }
        let result = await Self.run(bird, ["state"])
        guard result.status == 0, let data = result.out.data(using: .utf8) else {
            error = result.err.isEmpty ? "`bird state` failed" : result.err
            return
        }
        // Unchanged: don't publish, which would redraw the menu and icon.
        if data == lastStateData, state != nil { return }
        do {
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            state = try decoder.decode(BirdState.self, from: data)
            lastStateData = data
            error = nil
        } catch {
            self.error = "Unexpected output from `bird state`: \(error.localizedDescription)"
        }
    }

    // MARK: Watching ~/.bird

    /// Directories whose entries change when something the app shows changes:
    /// ~/.bird (config.toml is replaced on save), run/ (the daemon's socket
    /// and PHP-FPM pools' sockets and pid files) and php/ (installs).
    private var watchers: [String: DispatchSourceFileSystemObject] = [:]
    private var pendingRefresh: Task<Void, Never>?

    private func watchBirdHome() {
        let home = state.map { URL(fileURLWithPath: $0.birdHome) }
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".bird")
        for dir in [home, home.appendingPathComponent("run"), home.appendingPathComponent("php")] {
            let path = dir.path
            guard watchers[path] == nil else { continue }
            let fd = Darwin.open(path, O_EVTONLY)
            guard fd >= 0 else { continue }  // not there yet; tried again after the next refresh
            let source = DispatchSource.makeFileSystemObjectSource(
                fileDescriptor: fd, eventMask: [.write, .delete, .rename], queue: .main)
            source.setEventHandler { [weak self] in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    if !source.data.isDisjoint(with: DispatchSource.FileSystemEvent([.delete, .rename])) {
                        // The directory itself went away; watch it again once it's back.
                        source.cancel()
                        self.watchers[path] = nil
                    }
                    self.scheduleRefresh()
                }
            }
            source.setCancelHandler { close(fd) }
            source.resume()
            watchers[path] = source
        }
    }

    /// One refresh for a burst of changes (a save writes a temp file and renames it).
    private func scheduleRefresh() {
        pendingRefresh?.cancel()
        pendingRefresh = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 300_000_000)
            guard !Task.isCancelled else { return }
            await self?.refresh()
        }
    }

    /// Runs `bird <args>` and refreshes; failures show up in the UI.
    func perform(_ args: String...) {
        perform(args)
    }

    /// `activity` labels slow commands (installs) while they run; `cwd` is
    /// for commands that act on the current directory (`bird link`).
    func perform(_ args: [String], activity: String? = nil, cwd: URL? = nil, then: (() async -> Void)? = nil) {
        Task {
            busy = true
            self.activity = activity
            let result = await Self.run(bird, args, cwd: cwd)
            if result.status != 0 {
                let message = (result.err + result.out).trimmingCharacters(in: .whitespacesAndNewlines)
                error = message.isEmpty ? "bird \(args.joined(separator: " ")) failed" : message
            } else {
                error = nil
            }
            await refresh()
            await then?()
            self.activity = nil
            busy = false
        }
    }

    /// Loads the downloadable PHP versions and pending updates.
    func loadAvailable() async {
        let result = await Self.run(bird, ["state", "--available"])
        guard result.status == 0, let data = result.out.data(using: .utf8) else { return }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard let full = try? decoder.decode(BirdState.self, from: data) else { return }
        if let err = full.phpAvailableError {
            error = "Could not load available PHP versions: \(err)"
        }
        available = full.phpAvailable
        updates = Dictionary(uniqueKeysWithValues: full.phpVersions.compactMap { v in
            v.update.map { (v.minor, $0) }
        })
    }

    func runDoctor() {
        Task {
            busy = true
            let result = await Self.run(bird, ["doctor"])
            doctorOutput = (result.out + result.err).trimmingCharacters(in: .whitespacesAndNewlines)
            busy = false
        }
    }

    // The service runs as root under launchd, so starting and stopping it
    // goes through macOS's administrator prompt.

    func startService() {
        // Right after a stop, launchd may still be unloading the job; retry.
        let plist = Self.plist
        runAsAdmin("launchctl bootstrap system \(plist) || { sleep 1; launchctl bootstrap system \(plist); }")
    }

    func stopService() {
        runAsAdmin("launchctl bootout system/\(Self.launchdLabel)")
    }

    private func runAsAdmin(_ shell: String) {
        Task {
            busy = true
            let script = "do shell script \"\(shell)\" with administrator privileges"
            let result = await Self.run(URL(fileURLWithPath: "/usr/bin/osascript"), ["-e", script])
            // -128 = the user cancelled the password prompt.
            if result.status != 0 && !result.err.contains("-128") {
                error = result.err.trimmingCharacters(in: .whitespacesAndNewlines)
            }
            for _ in 0..<20 {
                try? await Task.sleep(nanoseconds: 250_000_000)
                await refresh()
                if (state?.running ?? false) == shell.contains("bootstrap") { break }
            }
            busy = false
        }
    }

    // MARK: Opening things

    func open(_ url: String) {
        if let url = URL(string: url) { NSWorkspace.shared.open(url) }
    }

    func reveal(_ path: String) {
        NSWorkspace.shared.open(URL(fileURLWithPath: path))
    }

    func openInTerminal(_ path: String) {
        let terminal = URL(fileURLWithPath: "/System/Applications/Utilities/Terminal.app")
        NSWorkspace.shared.open(
            [URL(fileURLWithPath: path)], withApplicationAt: terminal,
            configuration: NSWorkspace.OpenConfiguration())
    }

    func openLog() {
        guard let log = state?.log else { return }
        let console = URL(fileURLWithPath: "/System/Applications/Utilities/Console.app")
        NSWorkspace.shared.open(
            [URL(fileURLWithPath: log)], withApplicationAt: console,
            configuration: NSWorkspace.OpenConfiguration())
    }

    func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    // MARK: Updates

    /// Check for a new Bird release when the app starts (default on).
    var checkOnLaunch: Bool {
        get { UserDefaults.standard.object(forKey: Self.checkOnLaunchKey) as? Bool ?? true }
        set {
            UserDefaults.standard.set(newValue, forKey: Self.checkOnLaunchKey)
            objectWillChange.send()
        }
    }

    /// A newer release exists and the user hasn't chosen to skip it: shows
    /// the dot on the menu bar icon and the "Update to …" menu item.
    var updateWaiting: Bool {
        guard let update, update.available, let latest = update.latest else { return false }
        return latest != skippedVersion
    }

    var skippedVersion: String? {
        get { UserDefaults.standard.string(forKey: Self.skippedKey) }
        set {
            UserDefaults.standard.set(newValue, forKey: Self.skippedKey)
            objectWillChange.send()
        }
    }

    private func checkIfDue() async {
        guard checkOnLaunch else { return }
        let due = lastUpdateCheck.map { Date().timeIntervalSince($0) >= Self.checkInterval } ?? true
        if due { await checkForUpdates() }
    }

    /// "Check for Updates…" in the menu: checks, then answers in a dialog
    /// (no window), like Sparkle-based apps do.
    func checkForUpdatesInteractively() {
        Task {
            await checkForUpdates()
            if let error = updateError {
                showAlert(title: "Couldn't check for updates", text: error, style: .warning)
            } else if update?.available == true {
                presentUpdate()
            } else {
                showAlert(
                    title: "You're up to date",
                    text: "Bird \(update?.current ?? state?.birdVersion ?? "") is the latest version.",
                    style: .informational)
            }
        }
    }

    /// Dialog for an available update: Install Update, Later, Skip This Version.
    func presentUpdate() {
        guard let alert = makeUpdateAlert(), let latest = update?.latest else { return }
        NSApp.activate(ignoringOtherApps: true)
        switch alert.runModal() {
        case .alertFirstButtonReturn:
            installUpdate()
        case .alertThirdButtonReturn:
            skippedVersion = latest
        default:
            break
        }
    }

    func makeUpdateAlert() -> NSAlert? {
        guard let update, update.available, let latest = update.latest else { return nil }
        let alert = NSAlert()
        alert.messageText = "Bird \(latest) is available"
        var text = "You have Bird \(update.current). The update also updates Bird.app and restarts Bird."
        if let notes = update.notes?.trimmingCharacters(in: .whitespacesAndNewlines), !notes.isEmpty {
            text += "\n\n" + Self.plainNotes(notes)
        }
        alert.informativeText = text
        alert.addButton(withTitle: "Install Update")
        alert.addButton(withTitle: "Later")
        alert.addButton(withTitle: "Skip This Version")
        return alert
    }

    private func showAlert(title: String, text: String, style: NSAlert.Style) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = text
        alert.alertStyle = style
        alert.addButton(withTitle: "OK")
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }

    /// Release notes are Markdown; an alert shows plain text. Keeps the list,
    /// joins wrapped lines into paragraphs as Markdown does, drops the
    /// formatting, and caps the length.
    static func plainNotes(_ markdown: String) -> String {
        var lines: [String] = []
        var joinable = false  // the last line is paragraph or list text
        for raw in markdown
            .replacingOccurrences(of: "**", with: "")
            .replacingOccurrences(of: "`", with: "")
            .split(separator: "\n", omittingEmptySubsequences: false)
        {
            var l = raw.trimmingCharacters(in: .whitespaces)
            let heading = l.hasPrefix("#")
            while l.hasPrefix("#") { l.removeFirst() }
            l = l.trimmingCharacters(in: .whitespaces)
            let bullet = l.hasPrefix("- ") || l.hasPrefix("* ")
            if bullet { l = "• " + l.dropFirst(2) }
            if joinable, !l.isEmpty, !heading, !bullet {
                lines[lines.count - 1] += " " + l
            } else {
                lines.append(l)
            }
            joinable = !l.isEmpty && !heading
        }
        let text = lines.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
        return text.count > 900 ? String(text.prefix(900)) + "…" : text
    }

    func checkForUpdates() async {
        guard !checkingForUpdate else { return }
        checkingForUpdate = true
        defer { checkingForUpdate = false }
        let result = await Self.run(bird, ["self-update", "--check", "--json"])
        if result.status == 0, let data = result.out.data(using: .utf8),
           let info = try? JSONDecoder().decode(UpdateInfo.self, from: data)
        {
            update = info
            updateError = nil
            lastUpdateCheck = Date()
            UserDefaults.standard.set(lastUpdateCheck, forKey: Self.lastCheckKey)
        } else {
            let message = result.err.trimmingCharacters(in: .whitespacesAndNewlines)
            updateError = message.isEmpty ? "Could not check for updates" : message
        }
    }

    /// Runs `bird self-update` on its own: it replaces this app and starts
    /// the new one, so its output goes to a log file, not to a pipe that
    /// would break when this process exits.
    func installUpdate() {
        guard !updating else { return }
        let home = state.map { URL(fileURLWithPath: $0.birdHome) }
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".bird")
        let log = home.appendingPathComponent("log/update.log")
        try? FileManager.default.createDirectory(
            at: log.deletingLastPathComponent(), withIntermediateDirectories: true)
        FileManager.default.createFile(atPath: log.path, contents: nil)
        guard let handle = try? FileHandle(forWritingTo: log) else {
            updateError = "Could not write \(log.path)"
            return
        }
        let process = Process()
        process.executableURL = bird
        process.arguments = ["self-update"]
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = handle
        process.standardError = handle
        var env = ProcessInfo.processInfo.environment
        env["NO_COLOR"] = "1"
        // Update this copy of the app, wherever it is.
        if Bundle.main.bundleURL.pathExtension == "app" {
            env["BIRD_APP"] = Bundle.main.bundlePath
        }
        process.environment = env
        // Only reached when this app survives the update (e.g. it failed).
        process.terminationHandler = { [weak self] finished in
            let status = finished.terminationStatus
            let output = (try? String(contentsOf: log, encoding: .utf8)) ?? ""
            Task { @MainActor in
                guard let self else { return }
                self.updating = false
                self.activity = nil
                if status != 0 {
                    let tail = output.split(separator: "\n").suffix(3).joined(separator: "\n")
                    self.updateError = "Update failed: \(tail)"
                }
                await self.refresh()
                await self.checkForUpdates()
            }
        }
        do {
            try process.run()
            updating = true
            activity = "Updating Bird…"
            updateError = nil
        } catch {
            updateError = "Could not start the update: \(error.localizedDescription)"
        }
    }

    // MARK: Launch at login

    var launchAtLogin: Bool {
        get { SMAppService.mainApp.status == .enabled }
        set {
            do {
                if newValue {
                    try SMAppService.mainApp.register()
                } else {
                    try SMAppService.mainApp.unregister()
                }
            } catch {
                self.error = "Launch at login: \(error.localizedDescription)"
            }
            objectWillChange.send()
        }
    }

    // MARK: Process

    struct Result {
        let status: Int32
        let out: String
        let err: String
    }

    nonisolated static func run(_ executable: URL, _ args: [String], cwd: URL? = nil) async -> Result {
        await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let process = Process()
                process.executableURL = executable
                process.arguments = args
                if let cwd { process.currentDirectoryURL = cwd }
                let out = Pipe()
                let err = Pipe()
                process.standardOutput = out
                process.standardError = err
                process.standardInput = FileHandle.nullDevice
                // NO_COLOR: plain text in error messages shown in the menu.
                var env = ProcessInfo.processInfo.environment
                env["NO_COLOR"] = "1"
                process.environment = env
                do {
                    try process.run()
                } catch {
                    continuation.resume(returning: Result(status: -1, out: "", err: error.localizedDescription))
                    return
                }
                // Drain both pipes concurrently before waiting, so neither can
                // fill up and block the child.
                var errData = Data()
                let errDone = DispatchSemaphore(value: 0)
                DispatchQueue.global(qos: .userInitiated).async {
                    errData = err.fileHandleForReading.readDataToEndOfFile()
                    errDone.signal()
                }
                let outData = out.fileHandleForReading.readDataToEndOfFile()
                errDone.wait()
                process.waitUntilExit()
                continuation.resume(returning: Result(
                    status: process.terminationStatus,
                    out: String(decoding: outData, as: UTF8.self),
                    err: String(decoding: errData, as: UTF8.self)))
            }
        }
    }
}
