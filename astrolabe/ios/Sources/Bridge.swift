import Foundation

/// The bridge's HTTP API (the web UI itself uses the WebSocket instead).
/// The URL and the bridge's ASTROLABE_TOKEN (if you've set one — see
/// astrolabe/README.md) are both editable in Settings.app → Astrolabe.
enum Bridge {
    // Loopback, not a real bridge address — deliberately unreachable until
    // set in Settings.app, rather than a placeholder-looking string. Both
    // this and Settings.bundle's own DefaultValue need to stay valid URL
    // syntax: `URL(string:)` returns nil for "", and the force-unwrapped
    // fallback below would crash on first access to `baseURL` if this
    // were ever actually empty.
    static let defaultURL = "http://127.0.0.1:7979"

    static var baseURL: URL {
        let s = UserDefaults.standard.string(forKey: "bridge_url") ?? defaultURL
        return URL(string: s) ?? URL(string: defaultURL)!
    }

    /// nil when the field is empty — matches the bridge treating an unset
    /// ASTROLABE_TOKEN as "no auth required," not "auth with an empty
    /// string."
    static var token: String? {
        let s = UserDefaults.standard.string(forKey: "bridge_token") ?? ""
        return s.isEmpty ? nil : s
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
        if let token {
            req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        req.httpBody = try? JSONSerialization.data(withJSONObject: body)
        req.timeoutInterval = 15
        _ = try? await URLSession.shared.data(for: req)
    }
}
