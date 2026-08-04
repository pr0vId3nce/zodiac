import SwiftUI

struct ContentView: View {
    var body: some View {
        // The web app declares viewport-fit=cover and pads its own safe
        // areas, so the web view goes full-bleed.
        WebView()
            .ignoresSafeArea()
            .background(Color(red: 0x0b / 255, green: 0x10 / 255, blue: 0x20 / 255))
    }
}
