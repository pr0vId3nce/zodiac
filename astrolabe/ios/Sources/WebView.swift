import SwiftUI
import WebKit

struct WebView: UIViewRepresentable {
    @ObservedObject private var router = Router.shared

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.allowsInlineMediaPlayback = true
        let web = WKWebView(frame: .zero, configuration: config)
        web.navigationDelegate = context.coordinator
        web.isOpaque = false
        web.backgroundColor = UIColor(red: 0x0b / 255, green: 0x10 / 255, blue: 0x20 / 255, alpha: 1)
        web.scrollView.contentInsetAdjustmentBehavior = .never
        context.coordinator.web = web

        let refresh = UIRefreshControl()
        refresh.addTarget(
            context.coordinator, action: #selector(Coordinator.reload), for: .valueChanged)
        web.scrollView.refreshControl = refresh

        web.load(URLRequest(url: Bridge.baseURL))
        return web
    }

    func updateUIView(_ web: WKWebView, context: Context) {
        if let pane = router.pendingPane {
            context.coordinator.open(pane: pane)
            DispatchQueue.main.async { router.pendingPane = nil }
        }
    }

    final class Coordinator: NSObject, WKNavigationDelegate {
        weak var web: WKWebView?
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
            web?.load(URLRequest(url: Bridge.baseURL))
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
            webView.load(URLRequest(url: Bridge.baseURL))
        }
    }
}
