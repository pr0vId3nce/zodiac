import SwiftUI

struct ContentView: View {
    @ObservedObject private var store = ComputerStore.shared
    @ObservedObject private var router = Router.shared
    @State private var path: [Computer] = []

    var body: some View {
        NavigationStack(path: $path) {
            ComputerListView(store: store)
                .navigationDestination(for: Computer.self) { computer in
                    // The web app declares viewport-fit=cover and pads its
                    // own safe areas, so the web view goes full-bleed below
                    // the native bar that gets you back to the list.
                    WebView(computer: computer)
                        .ignoresSafeArea(edges: [.bottom, .horizontal])
                        .background(Color(red: 0x0b / 255, green: 0x10 / 255, blue: 0x20 / 255))
                        .navigationTitle(computer.name)
                        .navigationBarTitleDisplayMode(.inline)
                }
        }
        .onChange(of: router.pendingCid) { _, cid in
            navigateIfPending(cid)
        }
        .onAppear {
            navigateIfPending(router.pendingCid)
        }
    }

    /// Pushes to the right computer for a notification-driven deep link.
    /// Doesn't clear `pendingCid`/`pendingPane` itself — the WebView that
    /// ends up mounted there consumes them, see WebView.swift.
    private func navigateIfPending(_ cid: String?) {
        guard let cid, let computer = store.computer(cid: cid) else { return }
        path = [computer]
    }
}
