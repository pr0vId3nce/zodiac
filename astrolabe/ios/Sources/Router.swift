import Foundation

/// Carries "open pane N on computer C" from a notification tap through to
/// the right screen. Two-step consumption: ContentView's NavigationStack
/// uses `pendingCid` to push to that computer (cold launch or already
/// running, either way); the WebView mounted there then consumes
/// `pendingPane` itself once it confirms `pendingCid` still matches its
/// own computer, clearing both together — see WebView.swift.
@MainActor
final class Router: ObservableObject {
    static let shared = Router()
    @Published var pendingCid: String?
    @Published var pendingPane: Int?

    func open(cid: String, pane: Int) {
        pendingCid = cid
        pendingPane = pane
    }
}
