import UIKit
import UserNotifications

final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UserDefaults.standard.register(defaults: ["bridge_url": Bridge.defaultURL])

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
        Task { await Bridge.registerToken(token) }
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
        let pane = response.notification.request.content.userInfo["pane"] as? Int
        if let text = (response as? UNTextInputNotificationResponse)?.userText,
            response.actionIdentifier == "REPLY", let pane
        {
            await Bridge.reply(pane: pane, text: text)
        } else if response.actionIdentifier == UNNotificationDefaultActionIdentifier, let pane {
            await Router.shared.open(pane: pane)
        }
    }
}
