import Foundation

/// The bridge's HTTP API (the web UI itself uses the WebSocket instead).
/// The URL is editable in Settings.app → Astrolabe.
enum Bridge {
    static let defaultURL = "http://100.118.22.13:7979"

    static var baseURL: URL {
        let s = UserDefaults.standard.string(forKey: "bridge_url") ?? defaultURL
        return URL(string: s) ?? URL(string: defaultURL)!
    }

    static func registerToken(_ token: String) async {
        await post("/api/apns/register", ["token": token])
    }

    static func reply(pane: Int, text: String) async {
        await post("/api/prompt", ["pane": pane, "text": text])
    }

    private static func post(_ path: String, _ body: [String: Any]) async {
        guard let url = URL(string: path, relativeTo: baseURL) else { return }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try? JSONSerialization.data(withJSONObject: body)
        req.timeoutInterval = 15
        _ = try? await URLSession.shared.data(for: req)
    }
}
