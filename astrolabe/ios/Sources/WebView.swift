import SwiftUI
import WebKit

struct WebView: UIViewRepresentable {
    @ObservedObject private var router = Router.shared

    func makeCoordinator() -> Coordinator { Coordinator() }

    /// Bridge.baseURL with the configured token (if any) appended — the web
    /// app's own auth.ts reads `?t=` on boot the same way a browser-opened
    /// magic link does, so this is the one place the native shell needs to
    /// know about the token at all.
    private var loadURL: URL {
        guard let token = Bridge.token,
              var comps = URLComponents(url: Bridge.baseURL, resolvingAgainstBaseURL: false)
        else { return Bridge.baseURL }
        comps.queryItems = (comps.queryItems ?? []) + [URLQueryItem(name: "t", value: token)]
        return comps.url ?? Bridge.baseURL
    }

    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.allowsInlineMediaPlayback = true
        // Real haptics: native.ts's isNativeShell()/hapticTap() already
        // post {type:"haptic", kind} here — this is the one piece that was
        // missing to make that live instead of falling back to the
        // visual-pulse-only web path.
        config.userContentController.add(context.coordinator, name: "astrolabe")
        let web = WKWebView(frame: .zero, configuration: config)
        web.navigationDelegate = context.coordinator
        web.isOpaque = false
        web.backgroundColor = UIColor(red: 0x0b / 255, green: 0x10 / 255, blue: 0x20 / 255, alpha: 1)
        web.scrollView.contentInsetAdjustmentBehavior = .never
        context.coordinator.web = web
        context.coordinator.owner = self

        let refresh = UIRefreshControl()
        refresh.addTarget(
            context.coordinator, action: #selector(Coordinator.reload), for: .valueChanged)
        web.scrollView.refreshControl = refresh

        web.load(URLRequest(url: loadURL))
        return web
    }

    func updateUIView(_ web: WKWebView, context: Context) {
        if let pane = router.pendingPane {
            context.coordinator.open(pane: pane)
            DispatchQueue.main.async { router.pendingPane = nil }
        }
    }

    final class Coordinator: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
        weak var web: WKWebView?
        var owner: WebView?
        private var loaded = false
        private var deferredPane: Int?

        /// Route to a pane now, or after the page finishes loading (cold
        /// launch from a notification races the first page load).
        func open(pane: Int) {
            if loaded {
                web?.evaluateJavaScript("location.hash = '#/p/\(pane)'")
            } else {
                deferredPane = pane
            }
        }

        @objc func reload() {
            web?.load(URLRequest(url: owner?.loadURL ?? Bridge.baseURL))
            web?.scrollView.refreshControl?.endRefreshing()
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            loaded = true
            if let pane = deferredPane {
                deferredPane = nil
                webView.evaluateJavaScript("location.hash = '#/p/\(pane)'")
            }
        }

        func webView(
            _ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error
        ) {
            loaded = false
        }

        func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            loaded = false
            webView.load(URLRequest(url: owner?.loadURL ?? Bridge.baseURL))
        }

        /// Keep the persistent webview on the configured bridge host — pane
        /// content is untrusted text; nothing today auto-opens links from
        /// it, but there's no reason to leave the door open for a future
        /// feature that does.
        func webView(
            _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            guard let url = navigationAction.request.url,
                  let host = url.host,
                  host == Bridge.baseURL.host
            else {
                decisionHandler(.cancel)
                return
            }
            decisionHandler(.allow)
        }

        func userContentController(
            _ userContentController: WKUserContentController, didReceive message: WKScriptMessage
        ) {
            guard message.name == "astrolabe",
                  let body = message.body as? [String: Any],
                  body["type"] as? String == "haptic",
                  let kind = body["kind"] as? String
            else { return }
            DispatchQueue.main.async {
                switch kind {
                case "success":
                    UINotificationFeedbackGenerator().notificationOccurred(.success)
                case "warning":
                    UINotificationFeedbackGenerator().notificationOccurred(.warning)
                case "selection":
                    UISelectionFeedbackGenerator().selectionChanged()
                case "impact":
                    UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                default: // "tap" and anything unrecognized
                    UIImpactFeedbackGenerator(style: .light).impactOccurred()
                }
            }
        }
    }
}
