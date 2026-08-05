import SwiftUI

/// The app's first screen: every paired computer, tap to open its Herd/Pane
/// UI (WebView, unchanged), "+" to pair a new one.
struct ComputerListView: View {
    @ObservedObject var store: ComputerStore
    @State private var showScanner = false
    @State private var showManualAdd = false

    var body: some View {
        List {
            if store.computers.isEmpty {
                ContentUnavailableView(
                    "No computers paired",
                    systemImage: "qrcode",
                    description: Text(
                        "Scan a computer's pairing QR — press Alt+P on its zodiac home page.")
                )
                .listRowSeparator(.hidden)
            } else {
                ForEach(store.computers) { computer in
                    NavigationLink(value: computer) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(computer.name).font(.headline)
                            Text(computer.url.absoluteString)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .onDelete { offsets in
                    for i in offsets { store.remove(cid: store.computers[i].cid) }
                }
            }
        }
        .navigationTitle("Astrolabe")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Button {
                        showScanner = true
                    } label: {
                        Label("Scan QR", systemImage: "qrcode.viewfinder")
                    }
                    Button {
                        showManualAdd = true
                    } label: {
                        Label("Enter Manually", systemImage: "keyboard")
                    }
                } label: {
                    Image(systemName: "plus")
                }
            }
        }
        .sheet(isPresented: $showScanner) {
            ScannerSheet(store: store, isPresented: $showScanner)
        }
        .sheet(isPresented: $showManualAdd) {
            ManualAddView(store: store, isPresented: $showManualAdd)
        }
    }
}

/// Wraps QRScannerView with the parse-and-upsert glue, dismissing itself
/// once a valid pairing QR is decoded. An unrecognized QR (not one of
/// ours — a missing field from `Computer.parse`) is silently ignored
/// rather than surfacing an error alert for a stray scan of the wrong code.
private struct ScannerSheet: View {
    let store: ComputerStore
    @Binding var isPresented: Bool

    var body: some View {
        QRScannerView { payload in
            guard let computer = Computer.parse(pairingURL: payload) else { return }
            DispatchQueue.main.async {
                store.upsert(computer)
                isPresented = false
            }
        }
        .ignoresSafeArea()
        .overlay(alignment: .topTrailing) {
            Button {
                isPresented = false
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.title)
                    .foregroundStyle(.white, .black.opacity(0.5))
            }
            .padding()
        }
    }
}

/// Fallback for when the camera can't see the terminal — bad lighting, the
/// simulator, a remote/screenshared session. Same three fields a QR
/// encodes, typed by hand instead.
private struct ManualAddView: View {
    let store: ComputerStore
    @Binding var isPresented: Bool
    @State private var urlText = ""
    @State private var token = ""
    @State private var name = ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("URL (http://host:port)", text: $urlText)
                        .keyboardType(.URL)
                        .autocapitalization(.none)
                        .autocorrectionDisabled()
                    TextField("Token", text: $token)
                        .autocapitalization(.none)
                        .autocorrectionDisabled()
                    TextField("Name", text: $name)
                } footer: {
                    // No real cid is available without scanning — this
                    // mints a local one. Re-scanning this same machine's
                    // actual QR later adds a second entry rather than
                    // updating this one; deleting the stale manual entry
                    // is the workaround for that, not solved properly here.
                    Text("These are the same three values zodiac's Alt+P overlay encodes into its QR.")
                }
            }
            .navigationTitle("Add Manually")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { isPresented = false }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Add") {
                        guard let url = URL(string: urlText), !token.isEmpty else { return }
                        let displayName = name.isEmpty ? (url.host ?? urlText) : name
                        store.upsert(
                            Computer(
                                cid: UUID().uuidString, name: displayName, url: url, token: token,
                                lastSeen: nil))
                        isPresented = false
                    }
                    .disabled(urlText.isEmpty || token.isEmpty)
                }
            }
        }
    }
}
