import Foundation

/// One paired computer: a zodiac + astrolabe bridge pair, identified by the
/// stable `cid` the bridge mints once and persists (see
/// `astrolabe/bridge/identity.ts`) — not by `url`, which can change if the
/// tailscale IP does, and not by `token`, which rotates every time that
/// computer's zodiac server restarts (see `src/server.rs`'s
/// `gen_pairing_token`).
struct Computer: Codable, Identifiable, Hashable {
    var cid: String
    var name: String
    var url: URL
    var token: String
    var lastSeen: Date?

    var id: String { cid }

    /// Parses a scanned pairing URL — `http://<bridge>/?t=<token>&cid=<id>&name=<host>`,
    /// exactly what zodiac's Alt+P overlay encodes into the QR (see
    /// `src/client.rs`'s `draw_pair_overlay`). `nil` if any of the three
    /// query params zodiac always includes is missing, which only happens
    /// for garbage input (a QR that isn't one of ours) — not a real pairing
    /// code with a field dropped.
    static func parse(pairingURL raw: String) -> Computer? {
        guard let comps = URLComponents(string: raw),
              let scheme = comps.scheme, let host = comps.host
        else { return nil }
        var base = "\(scheme)://\(host)"
        if let port = comps.port { base += ":\(port)" }
        guard let baseURL = URL(string: base) else { return nil }
        func param(_ name: String) -> String? {
            comps.queryItems?.first { $0.name == name }?.value
        }
        guard let token = param("t"), let cid = param("cid"), let name = param("name")
        else { return nil }
        return Computer(cid: cid, name: name, url: baseURL, token: token, lastSeen: nil)
    }
}
