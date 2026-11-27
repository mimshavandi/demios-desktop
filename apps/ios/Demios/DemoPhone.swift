#if DEBUG
import Foundation

/// A stand-in for the Rust core, for screenshots and trying the screens without a Node.
/// Debug builds only: start the app with `-DemiosDemo ready|empty|offline|unpaired`, and
/// optionally `-DemiosDemoScreen pair|call|phone` to open that screen. Release builds always
/// use the real core.
enum DemoScreen: String { case pair, call, phone }

final class DemoPhone: PhoneProtocol, @unchecked Sendable {
    enum Mode: String { case ready, empty, offline, unpaired }

    private let mode: Mode
    private var paired: PairedNode?

    static var screen: DemoScreen? {
        let args = ProcessInfo.processInfo.arguments
        guard let i = args.firstIndex(of: "-DemiosDemoScreen"), i + 1 < args.count else { return nil }
        return DemoScreen(rawValue: args[i + 1])
    }

    static func fromLaunchArguments() -> DemoPhone? {
        let args = ProcessInfo.processInfo.arguments
        guard let i = args.firstIndex(of: "-DemiosDemo"), i + 1 < args.count,
              let mode = Mode(rawValue: args[i + 1]) else { return nil }
        return DemoPhone(mode: mode)
    }

    init(mode: Mode) {
        self.mode = mode
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        paired = mode == .unpaired
            ? nil
            : PairedNode(name: "Home server", address: "192.168.1.20:7443", pairedAt: now, lastReachedAt: now - 5 * 3_600_000)
    }

    func deviceId() -> String { "demo" }
    func pairedNode() throws -> PairedNode? { paired }

    func startPairing(linkText: String, phoneName: String) async throws -> [String] {
        try await Task.sleep(for: .milliseconds(600))
        return ["4821", "0937", "5512"]
    }

    func finishPairing() async throws -> PairedNode {
        try await Task.sleep(for: .seconds(2))
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let node = PairedNode(name: "Home server", address: "192.168.1.20:7443", pairedAt: now, lastReachedAt: now)
        paired = node
        return node
    }

    func cancelPairing() async {}

    func status() async throws -> NodeInfo {
        if mode == .offline { throw MobileError.Unreachable(address: "192.168.1.20:7443", reason: "timed out") }
        return NodeInfo(
            name: "Home server", version: "1.4.0", secretStoreLabel: "Encrypted file, key from a systemd credential",
            protectedAtRest: true, devices: 3, reachableFromOutside: false
        )
    }

    func waiting() async throws -> [HeldCall] {
        try await Task.sleep(for: .milliseconds(400))
        switch mode {
        case .offline: throw MobileError.Unreachable(address: "192.168.1.20:7443", reason: "timed out")
        case .empty, .unpaired: return []
        case .ready: return Self.calls
        }
    }

    func unpair() async throws -> Bool {
        paired = nil
        return mode != .offline
    }

    private static var calls: [HeldCall] {
        let now = Int64(Date().timeIntervalSince1970)
        return [
            HeldCall(
                id: "1", plugin: "Daily brief", job: "Morning summary", tool: "email.send",
                summary: "Send an email to 3 people",
                arguments: #"{"to":["ana@example.com","ben@example.com"],"cc":["team@example.com"],"bcc":[],"subject":"Today's summary","body":"Good morning,\n\nThree things moved overnight:\n- The supplier confirmed Friday's delivery.\n- Two invoices were paid.\n- The team meeting moved to 11:00.\n\nHave a good day.","attachments":[]}"#,
                argumentsSha256: "", createdAt: now - 2 * 3600, expiresAt: nil
            ),
            HeldCall(
                id: "2", plugin: "Inbox tidy", job: "Weekly clean-up", tool: "mail.delete_drafts",
                summary: "Delete 14 old drafts",
                arguments: #"{"older_than_days":90,"count":14}"#,
                argumentsSha256: "", createdAt: now - 5 * 3600, expiresAt: nil
            ),
        ]
    }
}
#endif
