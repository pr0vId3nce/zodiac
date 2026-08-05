import Foundation

/// The bridge's HTTP API (the web UI itself uses the WebSocket instead).
/// Every call takes an explicit `Computer` — there's no longer a single
/// global bridge, since the app now holds a whole `ComputerStore` list.
/// Each `Computer.token` is the pairing token obtained by scanning that
/// machine's QR, always sent as `Authorization: Bearer` (unlike the old
/// single-bridge model, pairing itself now implies having a token).
enum Bridge {
    static func registerToken(_ deviceToken: String, on computer: Computer) async {
        await post("/api/apns/register", ["token": deviceToken], on: computer)
    }

    static func unregisterToken(_ deviceToken: String, on computer: Computer) async {
        await post("/api/apns/unregister", ["token": deviceToken], on: computer)
    }

    static func reply(pane: Int, text: String, on computer: Computer) async {
        await post("/api/prompt", ["pane": pane, "text": text], on: computer)
    }

    private static func post(_ path: String, _ body: [String: Any], on computer: Computer) async {
        guard let url = URL(string: path, relativeTo: computer.url) else { return }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue("Bearer \(computer.token)", forHTTPHeaderField: "Authorization")
        req.httpBody = try? JSONSerialization.data(withJSONObject: body)
        req.timeoutInterval = 15
        _ = try? await URLSession.shared.data(for: req)
    }
}
