import SwiftUI
import UserNotifications

@main
struct AstrolabeApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            ContentView()
                .onChange(of: scenePhase) { _, phase in
                    if phase == .active {
                        // The bridge sets the badge to the needs-input count on
                        // each push; opening the app means you've seen it.
                        UNUserNotificationCenter.current().setBadgeCount(0)
                    }
                }
        }
    }
}
