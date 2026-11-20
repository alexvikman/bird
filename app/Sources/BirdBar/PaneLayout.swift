import AppKit
import SwiftUI

// The building blocks every pane of the main window is made of. A pane is a
// `PaneScaffold`: it draws the header, the margins, the window title and the
// toolbar, so no pane lays these out itself and they cannot drift apart.

enum Pane: String, CaseIterable, Identifiable {
    case sites, proxies, folders, php, settings, about
    var id: String { rawValue }

    var title: String {
        switch self {
        case .sites: "Sites"
        case .proxies: "Proxies"
        case .folders: "Folders"
        case .php: "PHP"
        case .settings: "Settings"
        case .about: "About"
        }
    }

    /// The name in the pane's header (About shows the app's).
    var headerTitle: String {
        self == .about ? "Bird" : title
    }

    /// What the pane is for, under its header.
    func summary(tld: String) -> String {
        switch self {
        case .sites:
            "Every project Bird serves, linked or in a parked folder. HTTPS always works; the switch also redirects HTTP to HTTPS."
        case .proxies:
            "Serve a .\(tld) domain from another server, such as Vite or Docker, with HTTPS and WebSockets."
        case .folders:
            "Every folder inside a parked folder is served as <folder>.\(tld). Nothing in them is changed."
        case .php:
            "Install, update and switch between PHP versions, and set the limits PHP runs with."
        case .settings:
            "The global PHP version, Opcache and Xdebug, where Bird keeps its files, and a check that everything works."
        case .about:
            "Local PHP sites on .\(tld), and nothing more."
        }
    }

    /// The hover text of the pane's Refresh button.
    var refreshHelp: String {
        switch self {
        case .sites: "Reload the list of sites"
        case .proxies: "Reload the list of proxies"
        case .folders: "Reload the parked folders"
        case .php: "Reload the installed versions and check for new PHP versions and builds"
        case .settings: "Reload Bird's status and settings"
        case .about: "Check for a new version of Bird"
        }
    }

    var icon: String {
        switch self {
        case .sites: "globe"
        case .proxies: "arrow.left.arrow.right"
        case .folders: "folder.fill"
        case .php: "chevron.left.forwardslash.chevron.right"
        case .settings: "gearshape.fill"
        case .about: "info"
        }
    }

    var color: Color {
        switch self {
        case .sites: .blue
        case .proxies: .indigo
        case .folders: .cyan
        case .php: Color(red: 0.47, green: 0.48, blue: 0.70)  // PHP's own purple
        case .settings: .gray
        case .about: .gray
        }
    }
}

/// A white symbol on a colored rounded square, like System Settings; About
/// shows the app icon. 22 points in the sidebar, larger in a pane's header.
struct PaneIcon: View {
    let pane: Pane
    var size: CGFloat = 22

    var body: some View {
        if pane == .about {
            // The icon has transparent margins (the macOS icon grid); crop
            // them so it lines up with the squares.
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .frame(width: size * 27 / 22, height: size * 27 / 22)
                .frame(width: size, height: size)
        } else {
            Image(systemName: pane.icon)
                .font(.system(size: size / 2, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: size, height: size)
                .background(
                    RoundedRectangle(cornerRadius: size * 0.27, style: .continuous)
                        .fill(pane.color.gradient))
        }
    }
}

/// A toolbar button of a pane. Every one has a hover text.
struct PaneAction: Identifiable {
    var id: String { title }
    let title: String
    let icon: String
    let help: String
    var disabled = false
    let run: () -> Void
}

enum PaneMetrics {
    static let side: CGFloat = 28
    static let top: CGFloat = 30
    static let bottom: CGFloat = 20
    static let spacing: CGFloat = 20
    static let corner: CGFloat = 12
}

/// A pane: header card, then the content, in one of two layouts.
struct PaneScaffold<Extra: View, Content: View>: View {
    enum Layout {
        /// Cards of rows that scroll, header included.
        case cards
        /// A table in a card with a title, filling the space below the
        /// header, so it starts as far below the header as the first card.
        case table(title: String)
    }

    let pane: Pane
    let tld: String
    var layout: Layout = .cards
    /// The pane's own buttons, left of Refresh.
    var actions: [PaneAction] = []
    /// What the Refresh button, last in every pane's toolbar, reloads.
    let refresh: () async -> Void
    /// Lines under the header's text (About's version).
    @ViewBuilder var headerExtra: Extra
    @ViewBuilder var content: Content

    var body: some View {
        Group {
            switch layout {
            case .cards:
                ScrollView {
                    VStack(alignment: .leading, spacing: PaneMetrics.spacing) {
                        header
                        content
                    }
                    .padding(.horizontal, PaneMetrics.side)
                    .padding(.top, PaneMetrics.top)
                    .padding(.bottom, PaneMetrics.bottom)
                }
            case .table(let title):
                VStack(alignment: .leading, spacing: PaneMetrics.spacing) {
                    header
                    Card(title: title) {
                        content
                            .tableStyle(.inset(alternatesRowBackgrounds: false))
                            .scrollContentBackground(.hidden)
                            .frame(maxHeight: .infinity)
                            .clipShape(RoundedRectangle(cornerRadius: PaneMetrics.corner, style: .continuous))
                    }
                }
                .padding(.horizontal, PaneMetrics.side)
                .padding(.top, PaneMetrics.top)
                .padding(.bottom, PaneMetrics.bottom)
            }
        }
        .navigationTitle(pane.title)
        // Every pane has at least the Refresh button: with an empty toolbar
        // macOS shrinks it to a plain title bar, with the title centered and
        // the pane moved up, so that pane would not look like the others.
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                ForEach(actions) { action in
                    Button(action: action.run) {
                        Label(action.title, systemImage: action.icon)
                    }
                    .help(action.help)
                    .disabled(action.disabled)
                }
                Button {
                    Task { await refresh() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .help(pane.refreshHelp)
            }
        }
    }

    private var header: some View {
        VStack(spacing: 6) {
            PaneIcon(pane: pane, size: 64)
                .padding(.bottom, 4)
            Text(pane.headerTitle).font(.title2.weight(.semibold))
            Text(pane.summary(tld: tld))
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 460)
            headerExtra
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 20)
        .padding(.horizontal, 10)
        .background(CardBox())
    }
}

extension PaneScaffold where Extra == EmptyView {
    init(
        pane: Pane, tld: String, layout: Layout = .cards, actions: [PaneAction] = [],
        refresh: @escaping () async -> Void, @ViewBuilder content: () -> Content
    ) {
        self.init(
            pane: pane, tld: tld, layout: layout, actions: actions, refresh: refresh,
            headerExtra: { EmptyView() }, content: content)
    }
}

// MARK: - Cards

/// The grey rounded box behind a group of rows (and the header).
struct CardBox: View {
    var body: some View {
        RoundedRectangle(cornerRadius: PaneMetrics.corner, style: .continuous)
            .fill(Color.primary.opacity(0.045))
    }
}

/// Rows in a box, with an optional title above it and footnote below.
/// Separate rows with `CardDivider()`; lay a row out with `.cardRow()` or
/// `SettingRow`.
struct Card<Content: View>: View {
    var title: String?
    var footer: String?
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let title {
                Text(title).font(.headline).padding(.leading, 10)
            }
            VStack(alignment: .leading, spacing: 0) {
                content
            }
            .background(CardBox())
            if let footer {
                Text(footer)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .trailing)
                    .padding(.horizontal, 10)
            }
        }
    }
}

struct CardDivider: View {
    var body: some View {
        Divider().padding(.horizontal, 10)
    }
}

extension View {
    /// The insets of a row in a `Card`.
    func cardRow() -> some View {
        frame(maxWidth: .infinity, minHeight: 22, alignment: .leading)
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
    }

    /// Hides the button that collapses the sidebar (macOS 14 and later).
    @ViewBuilder
    func withoutSidebarToggle() -> some View {
        if #available(macOS 14.0, *) {
            toolbar(removing: .sidebarToggle)
        } else {
            self
        }
    }
}

/// A card row: a title (and caption) on the left, a control on the right.
struct SettingRow<Control: View>: View {
    let title: String
    var caption: String?
    var help: String?
    @ViewBuilder let control: Control

    var body: some View {
        HStack(alignment: .center) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                if let caption {
                    Text(caption)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 16)
            control
        }
        .cardRow()
        .help(help ?? "")
    }
}

/// A switch as a card row.
struct ToggleRow: View {
    let title: String
    var caption: String?
    var help: String?
    let isOn: Binding<Bool>

    var body: some View {
        SettingRow(title: title, caption: caption, help: help) {
            Toggle(title, isOn: isOn)
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.small)
        }
    }
}
