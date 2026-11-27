import SwiftUI

@main
struct DemiosApp: App {
    @State private var model: AppModel
    @Environment(\.scenePhase) private var phase

    init() {
        #if DEBUG
        if let demo = DemoPhone.fromLaunchArguments() {
            _model = State(initialValue: AppModel(phone: demo))
            return
        }
        #endif
        _model = State(initialValue: AppModel())
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                // The app switcher shows no content (ADR-0019 §9).
                .overlay {
                    if phase != .active { PrivacyCover() }
                }
        }
    }
}

struct RootView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if let error = model.openError {
            OpenFailedView(message: error)
        } else if model.paired == nil {
            WelcomeView()
        } else {
            WaitingView()
        }
    }
}

/// Covers the screen while the app isn't active, so the app switcher shows no content.
struct PrivacyCover: View {
    var body: some View {
        ZStack {
            Color(uiColor: .systemBackground).ignoresSafeArea()
            Text("Demios").font(.largeTitle.bold())
        }
        .accessibilityHidden(true)
    }
}

/// The Keychain or this phone's store couldn't be opened. Nothing is replaced because of it.
struct OpenFailedView: View {
    @Environment(AppModel.self) private var model
    let message: String

    var body: some View {
        ContentUnavailableView {
            Label("Demios", systemImage: "exclamationmark.octagon")
        } description: {
            Text(message)
        } actions: {
            Button("Try again") {
                model.open()
                model.reloadPaired()
            }
            .buttonStyle(.borderedProminent)
        }
    }
}

// MARK: - Formatting

enum Format {
    /// "07:00" today, otherwise "26 Nov, 07:00".
    static func when(_ date: Date, now: Date = Date()) -> String {
        let time = date.formatted(date: .omitted, time: .shortened)
        if Calendar.current.isDate(date, inSameDayAs: now) { return time }
        return "\(date.formatted(.dateTime.day().month(.abbreviated))), \(time)"
    }

    /// How long something has waited: "now", "12 min", "2 h", "3 d".
    static func age(since date: Date, now: Date = Date()) -> String {
        let minutes = Int(now.timeIntervalSince(date) / 60)
        if minutes < 1 { return "now" }
        if minutes < 60 { return "\(minutes) min" }
        if minutes < 60 * 24 { return "\(minutes / 60) h" }
        return "\(minutes / (60 * 24)) d"
    }

    static func date(unixSeconds s: Int64) -> Date { Date(timeIntervalSince1970: TimeInterval(s)) }
    static func date(unixMillis ms: Int64) -> Date { Date(timeIntervalSince1970: TimeInterval(ms) / 1000) }

    /// "192.168.1.20" from "192.168.1.20:7443" (and "[fe80::1]" from "[fe80::1]:7443").
    static func host(_ address: String) -> String {
        guard let colon = address.lastIndex(of: ":"), !address.hasSuffix("]") else { return address }
        return String(address[..<colon])
    }
}

extension Color {
    static let dnSuccess = Color("Success")
    static let dnWarning = Color("Warning")
    static let dnDanger = Color("Danger")
}
