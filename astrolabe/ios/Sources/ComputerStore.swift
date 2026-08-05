import Foundation

/// The list of paired computers — the app's actual multi-computer state.
/// Deliberately plain `UserDefaults`, not `Settings.bundle`: this list is
/// built by scanning QR codes, not by hand-editing in Settings.app.
@MainActor
final class ComputerStore: ObservableObject {
    static let shared = ComputerStore()

    @Published private(set) var computers: [Computer] = []

    /// Set once by AppDelegate after APNs registration succeeds — lets
    /// remove(cid:) also unregister this device from the bridge being
    /// dropped, not just forget it locally and leave it pushing forever.
    var deviceToken: String?

    private let key = "computers"

    private init() {
        load()
    }

    private func load() {
        guard let data = UserDefaults.standard.data(forKey: key),
              let decoded = try? JSONDecoder().decode([Computer].self, from: data)
        else { return }
        computers = decoded
    }

    private func save() {
        guard let data = try? JSONEncoder().encode(computers) else { return }
        UserDefaults.standard.set(data, forKey: key)
    }

    func computer(cid: String) -> Computer? {
        computers.first { $0.cid == cid }
    }

    /// Adds a newly-scanned (or manually-entered) computer, or — matching
    /// by `cid` — updates an already-known one in place. This is how a
    /// rescanned, rotated token replaces a stale one without the list
    /// growing a duplicate entry for the same machine.
    func upsert(_ scanned: Computer) {
        var c = scanned
        c.lastSeen = Date()
        if let i = computers.firstIndex(where: { $0.cid == c.cid }) {
            computers[i] = c
        } else {
            computers.append(c)
        }
        save()
        // Register push for this computer right away if we already have a
        // device token from an earlier launch — otherwise it'd wait for
        // the next didRegisterForRemoteNotificationsWithDeviceToken, which
        // may not come again this session.
        if let deviceToken {
            Task { await Bridge.registerToken(deviceToken, on: c) }
        }
    }

    func remove(cid: String) {
        if let deviceToken, let computer = computer(cid: cid) {
            Task { await Bridge.unregisterToken(deviceToken, on: computer) }
        }
        computers.removeAll { $0.cid == cid }
        save()
    }
}
