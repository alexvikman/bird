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
        Task { await refresh() }
        // Keep the menu bar icon roughly in sync while the menu is closed.
        // The menu is a native menu that can't refresh when it opens, so poll.
        Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { [weak self] _ in
            Task { await self?.refresh() }
        }
    }

    func refresh() async {
        guard !refreshing else { return }
        refreshing = true
        defer { refreshing = false }
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
        do {
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            state = try decoder.decode(BirdState.self, from: data)
            error = nil
        } catch {
            self.error = "Unexpected output from `bird state`: \(error.localizedDescription)"
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
