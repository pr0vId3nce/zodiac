import Foundation

/// Carries "open pane N" from a notification tap into the web view.
@MainActor
final class Router: ObservableObject {
    static let shared = Router()
    @Published var pendingPane: Int?

    func open(pane: Int) {
        pendingPane = pane
    }
}
