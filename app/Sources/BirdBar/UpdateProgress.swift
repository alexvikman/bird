import AppKit
import SwiftUI

/// How an update that this app is still around to see ended. A successful
/// update usually never gets here: `bird self-update` quits this app and
/// starts the new one.
enum UpdateOutcome: Equatable {
    case failed(String)
    /// Done, but this app was not replaced (the lines are its warnings).
    case finished([String])
}

/// The steps of `bird self-update`, read from what it writes to update.log:
/// each part writes its line when it is done. It downloads and installs the
/// bird command and Bird.app one after the other, shown as one step.
struct UpdateStep: Identifiable {
    enum Status { case done, active, pending }
    let id: Int
    let title: String
    let status: Status

    /// The lines that mark each part done, in order.
    private static let marks: [(String) -> Bool] = [
        { $0.hasSuffix("release signature verified") },
        { $0.hasPrefix("↓ bird ") },
        { $0.hasPrefix("✓ bird ") },
        { $0.hasPrefix("↓ Bird.app") },
        { $0.hasPrefix("✓ updated ") },
    ]

    private static func done(_ log: String) -> [Bool] {
        let lines = log.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }
        return marks.map { mark in lines.contains(where: mark) }
    }

    static func steps(log: String, version: String) -> [UpdateStep] {
        let done = done(log)
        // Each step is done when the last of its parts is.
        let visible: [(String, Int)] = [
            ("Checking the release signature", 0),
            ("Downloading and installing Bird \(version)", 3),
            ("Restarting Bird", 4),
        ]
        let active = visible.firstIndex { !done[$0.1] }
        return visible.enumerated().map { i, step in
            UpdateStep(
                id: i, title: step.0,
                status: done[step.1] ? .done : (i == active ? .active : .pending))
        }
    }

    /// 0…1 over all the parts, so the bar moves through the long middle step.
    static func progress(log: String) -> Double {
        let done = done(log).filter { $0 }.count
        return min(1, (Double(done) + 0.5) / Double(marks.count))
    }
}

/// The update window's content, like Sparkle's: what is happening now, and
/// what went wrong if it did.
struct UpdateProgressView: View {
    let version: String
    let log: String
    let outcome: UpdateOutcome?
    var showLog: () -> Void = {}
    var close: () -> Void = {}

    var body: some View {
        let steps = UpdateStep.steps(log: log, version: version)
        HStack(alignment: .top, spacing: 16) {
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .frame(width: 64, height: 64)
            VStack(alignment: .leading, spacing: 12) {
                Text(title).font(.headline)
                if outcome == nil {
                    ProgressView(value: UpdateStep.progress(log: log))
                }
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(steps) { step in
                        HStack(spacing: 8) {
                            icon(step.status)
                                .frame(width: 16)
                            Text(step.title)
                                .foregroundStyle(step.status == .pending ? .secondary : .primary)
                        }
                    }
                }
                footer
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(20)
        .frame(width: 420)
    }

    private var title: String {
        switch outcome {
        case nil: "Updating Bird to \(version)…"
        case .failed: "The update didn't finish"
        case .finished: "Bird is updated to \(version)"
        }
    }

    @ViewBuilder
    private func icon(_ status: UpdateStep.Status) -> some View {
        switch status {
        case .done:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .active where outcome == nil:
            ProgressView().controlSize(.small)
        case .active:
            Image(systemName: "xmark.circle.fill").foregroundStyle(.red)
        case .pending:
            Image(systemName: "circle").foregroundStyle(.tertiary)
        }
    }

    @ViewBuilder
    private var footer: some View {
        switch outcome {
        case nil:
            Text("Bird restarts by itself when it's done.")
                .font(.caption)
                .foregroundStyle(.secondary)
        case .failed(let message):
            Text(message)
                .font(.callout)
                .foregroundStyle(.red)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            buttons
        case .finished(let warnings):
            ForEach(warnings, id: \.self) { warning in
                Text(warning)
                    .font(.callout)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
            buttons
        }
    }

    private var buttons: some View {
        HStack {
            Button("Show Log", action: showLog)
            Spacer()
            Button("Close", action: close).keyboardShortcut(.defaultAction)
        }
    }
}

/// Follows the model while the update runs.
struct UpdateProgressWindowContent: View {
    @ObservedObject var model: BirdModel

    var body: some View {
        UpdateProgressView(
            version: model.updatingTo ?? "",
            log: model.updateLog,
            outcome: model.updateOutcome,
            showLog: { model.openUpdateLog() },
            close: { model.closeUpdateProgress() })
    }
}
