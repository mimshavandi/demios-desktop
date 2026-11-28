import SwiftUI

/// 3 · Waiting, the home screen (design: PhoneCompanion): the held calls on the Node,
/// oldest first. Read-only in this version.
struct WaitingView: View {
    @Environment(AppModel.self) private var model
    @State private var path: [Route] = []

    enum Route: Hashable {
        case call(HeldCall)
        case phone
    }

    var body: some View {
        NavigationStack(path: $path) {
            List {
                Section {
                    content
                } header: {
                    subtitle.textCase(nil)
                } footer: {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Answer on your computer for now. Approving from this phone comes next.")
                        Text("Notifications are off. Open the app to see what's waiting.")
                    }
                    .padding(.top, 8)
                }
            }
            .navigationTitle("Waiting")
            .navigationDestination(for: Route.self) { route in
                switch route {
                case .call(let call): HeldCallView(call: call)
                case .phone: ThisPhoneView()
                }
            }
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    NavigationLink(value: Route.phone) {
                        Image(systemName: "gearshape")
                    }
                    .accessibilityLabel("This phone")
                }
            }
            .refreshable { await model.refresh() }
            .task {
                await model.refresh()
                #if DEBUG
                switch DemoPhone.screen {
                case .call: if case .ready(let calls) = model.waiting, let first = calls.first { path = [.call(first)] }
                case .phone: path = [.phone]
                default: break
                }
                #endif
            }
        }
    }

    /// "Home server · Online" or "Home server · Offline since 03:12".
    @ViewBuilder private var subtitle: some View {
        let name = model.node?.name ?? model.paired?.name ?? "Your Node"
        switch model.waiting {
        case .loading:
            Text(name)
        case .ready:
            HStack(spacing: 4) {
                Text("\(name) ·")
                Label("Online", systemImage: "checkmark.circle").foregroundStyle(Color.dnSuccess)
            }
            .accessibilityElement(children: .combine)
        case .failed:
            if let at = model.paired?.lastReachedAt {
                Text("\(name) · Offline since \(Format.when(Format.date(unixMillis: at)))")
            } else {
                Text("\(name) · Offline")
            }
        }
    }

    @ViewBuilder private var content: some View {
        switch model.waiting {
        case .loading:
            ForEach(0..<3, id: \.self) { _ in
                HeldCallRow(call: .placeholder).redacted(reason: .placeholder)
            }
            .accessibilityHidden(true)
        case .ready(let calls):
            if calls.isEmpty {
                ContentUnavailableView(
                    "Nothing is waiting.",
                    systemImage: "tray",
                    description: Text("When a job on your Node needs your OK, it shows up here.")
                )
            } else {
                rows(calls)
            }
        case .failed(let reason, let unreachable, let last, let asOf):
            VStack(alignment: .leading, spacing: 8) {
                Label {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Couldn't reach your Node.").bold()
                        Text(unreachable ? "It may be off, or this phone isn't on its network." : reason)
                        if let asOf { Text("As of \(Format.when(asOf)).").foregroundStyle(.secondary) }
                    }
                } icon: {
                    Image(systemName: "exclamationmark.octagon").foregroundStyle(Color.dnDanger)
                }
                Button("Try again") { Task { await model.refresh() } }
                    .buttonStyle(.bordered)
            }
            .padding(.vertical, 4)
            if let last { rows(last) }
        }
    }

    private func rows(_ calls: [HeldCall]) -> some View {
        ForEach(calls, id: \.id) { call in
            NavigationLink(value: Route.call(call)) { HeldCallRow(call: call) }
        }
    }
}

/// One held call: the add-on and job, what it wants, and how long it has waited. The risk
/// badge comes once the Node sends a risk with each call (#325).
struct HeldCallRow: View {
    let call: HeldCall

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("\(call.plugin) · \(call.job)")
                .font(.caption)
                .foregroundStyle(.secondary)
            HStack(alignment: .firstTextBaseline) {
                Text(call.summary).font(.body.weight(.medium))
                Spacer()
                Text(Format.age(since: Format.date(unixSeconds: call.createdAt)))
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(call.summary). \(call.plugin), \(call.job). Waiting \(Format.age(since: Format.date(unixSeconds: call.createdAt)))."
        )
    }
}

extension HeldCall {
    /// The shape of a row while the list loads (redacted, never shown as text).
    static let placeholder = HeldCall(
        id: "", plugin: "Add-on name", job: "Job name", tool: "", summary: "What the job wants to do",
        arguments: "{}", argumentsSha256: "", createdAt: Int64(Date().timeIntervalSince1970), expiresAt: nil
    )
}
