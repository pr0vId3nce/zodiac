import UIKit
import UserNotifications

final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        let center = UNUserNotificationCenter.current()
        center.delegate = self

        // Long-press a "needs you" notification → dictate/type → Send,
        // without ever opening the app.
        let reply = UNTextInputNotificationAction(
            identifier: "REPLY",
            title: "Reply",
            options: [],
            textInputButtonTitle: "Send",
            textInputPlaceholder: "answer the agent…"
        )
        center.setNotificationCategories([
            UNNotificationCategory(
                identifier: "AGENT_PROMPT",
                actions: [reply],
                intentIdentifiers: [],
                options: []
            )
        ])

        Task {
            let granted =
                (try? await center.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
            if granted {
                await MainActor.run { UIApplication.shared.registerForRemoteNotifications() }
            }
        }
        return true
    }

    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        let token = deviceToken.map { String(format: "%02x", $0) }.joined()
        Task { @MainActor in
            let store = ComputerStore.shared
            store.deviceToken = token
            // Every paired computer runs its own independent bridge — the
            // same physical device token has to be registered with each
            // one separately, best-effort (one unreachable bridge
            // shouldn't block the rest).
            for computer in store.computers {
                await Bridge.registerToken(token, on: computer)
            }
        }
    }

    func application(
        _ application: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        print("astrolabe: APNs registration failed: \(error)")
    }

    // Keep banners visible while the app is foregrounded.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .sound]
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        let info = response.notification.request.content.userInfo
        let pane = info["pane"] as? Int
        let cid = info["cid"] as? String
        guard let pane, let cid, let computer = await ComputerStore.shared.computer(cid: cid)
        else { return }
        if let text = (response as? UNTextInputNotificationResponse)?.userText,
            response.actionIdentifier == "REPLY"
        {
            await Bridge.reply(pane: pane, text: text, on: computer)
        } else if response.actionIdentifier == UNNotificationDefaultActionIdentifier {
            await Router.shared.open(cid: cid, pane: pane)
        }
    }
}
