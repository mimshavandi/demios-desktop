import Foundation
import Observation
import UIKit

/// The words the core gives for an error (the same as on the desktop).
func message(_ error: Error) -> String {
    if let e = error as? MobileError { return errorMessage(error: e) }
    return error.localizedDescription
}

/// What the Waiting list shows.
enum Waiting {
    case loading
    case ready([HeldCall])
    /// The Node didn't answer. `last` is the list from the last time it did, if any.
    case failed(String, unreachable: Bool, last: [HeldCall]?, asOf: Date?)
}

/// The app's state over the Rust core. Everything runs on the main actor; the core's calls
/// are async and do their network work off it.
@MainActor
@Observable
final class AppModel {
    /// `nil` when the core couldn't open (the Keychain or the store failed).
    private(set) var phone: (any PhoneProtocol)?
    private(set) var openError: String?
    private(set) var paired: PairedNode?
    private(set) var node: NodeInfo?
    private(set) var online = false
    private(set) var waiting: Waiting = .loading
    private var lastList: (calls: [HeldCall], at: Date)?
    /// Set when this phone unpaired but the Node didn't answer, so it still lists it.
    var nodeStillListsPhone = false

    /// This phone's name on the Node's device list, as sent when pairing.
    var phoneName: String {
        get { UserDefaults.standard.string(forKey: "phoneName") ?? UIDevice.current.name }
        set { UserDefaults.standard.set(newValue, forKey: "phoneName") }
    }

    init(phone: (any PhoneProtocol)? = nil) {
        if let phone {
            self.phone = phone
        } else {
            open()
        }
        reloadPaired()
    }

    func open() {
        do {
            phone = try Phone.open(store: Keychain(), dbPath: try Self.storePath())
            openError = nil
        } catch {
            phone = nil
            openError = message(error)
        }
    }

    /// Application Support/demios-phone.db, readable only while the phone is unlocked.
    private static func storePath() throws -> String {
        let fm = FileManager.default
        let dir = try fm.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
        try fm.setAttributes([.protectionKey: FileProtectionType.complete], ofItemAtPath: dir.path)
        return dir.appendingPathComponent("demios-phone.db").path
    }

    func reloadPaired() {
        guard let phone else { return }
        do {
            paired = try phone.pairedNode()
        } catch {
            openError = message(error)
        }
    }

    /// Asks the Node for its status and what's waiting. When it doesn't answer, the last
    /// list stays, marked with its time.
    func refresh() async {
        guard let phone, paired != nil else { return }
        if case .failed = waiting {} else if lastList == nil { waiting = .loading }
        do {
            async let info = phone.status()
            async let calls = phone.waiting()
            let (i, c) = try await (info, calls)
            node = i
            online = true
            lastList = (c, Date())
            waiting = .ready(c)
        } catch {
            online = false
            let unreachable: Bool
            if case MobileError.Unreachable = error { unreachable = true } else { unreachable = false }
            waiting = .failed(message(error), unreachable: unreachable, last: lastList?.calls, asOf: lastList?.at)
        }
        reloadPaired()
    }

    func paired(with node: PairedNode) {
        paired = node
        waiting = .loading
        lastList = nil
    }

    /// Unpairs. When the Node doesn't confirm, `nodeStillListsPhone` says so.
    func unpair() async throws {
        guard let phone else { return }
        let confirmed = try await phone.unpair()
        nodeStillListsPhone = !confirmed
        paired = nil
        node = nil
        online = false
        lastList = nil
        waiting = .loading
    }
}
