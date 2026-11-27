import AVFoundation
import SwiftUI
import VisionKit

/// 2 · Pair (design: PhoneCompanion): scan the code or paste the link, then compare the
/// check numbers and confirm on the Node. A mismatch stops pairing and nothing is shared.
struct PairView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    enum Step: Equatable {
        case scan
        case paste
        case connecting(String)
        case numbers([String])
    }

    @State private var step: Step = PairView.canScan ? .scan : .paste
    @State private var link = ""
    @State private var error: String?
    @State private var camera = AVCaptureDevice.authorizationStatus(for: .video)
    /// Answers that arrive after the person cancelled are ignored.
    @State private var attempt = 0

    static var canScan: Bool {
        DataScannerViewController.isSupported && DataScannerViewController.isAvailable
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    switch step {
                    case .scan: scan
                    case .paste: paste
                    case .connecting(let text): connecting(text)
                    case .numbers(let numbers): check(numbers)
                    }
                }
                .padding()
            }
            .navigationTitle(isNumbers ? "Check the numbers" : "Pair with your Node")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                if !isNumbers {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { stop() }
                    }
                }
            }
        }
        .interactiveDismissDisabled(isNumbers)
        #if DEBUG
        .onAppear {
            // `-DemiosPairLink <link>` pastes a link and connects, for testing without a camera.
            let args = ProcessInfo.processInfo.arguments
            if let i = args.firstIndex(of: "-DemiosPairLink"), i + 1 < args.count, step == .paste {
                link = args[i + 1]
                connect()
            }
        }
        #endif
    }

    private var isNumbers: Bool {
        if case .numbers = step { return true }
        return false
    }

    // MARK: Scan

    @ViewBuilder private var scan: some View {
        switch camera {
        case .denied, .restricted:
            Text("Demios can't use the camera. Paste the link instead, or allow the camera in Settings.")
            pasteInstead
        default:
            Scanner { text in
                guard step == .scan, text.hasPrefix("demios://pair") else { return }
                link = text
                connect()
            } failed: { reason in
                error = reason
            }
            .frame(height: 280)
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .accessibilityLabel("Camera")
            .task {
                if camera == .notDetermined {
                    _ = await AVCaptureDevice.requestAccess(for: .video)
                    camera = AVCaptureDevice.authorizationStatus(for: .video)
                }
            }
            Text("Scan the code your Node shows.").foregroundStyle(.secondary)
            errorText
            pasteInstead
        }
    }

    private var pasteInstead: some View {
        Button("Paste link instead") {
            error = nil
            step = .paste
        }
        .frame(maxWidth: .infinity, minHeight: 44)
    }

    // MARK: Paste

    @ViewBuilder private var paste: some View {
        TextField("Pairing link", text: $link, prompt: Text("demios://pair?…"), axis: .vertical)
            .textFieldStyle(.roundedBorder)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
            .keyboardType(.URL)
            .submitLabel(.go)
            .onSubmit(connect)
        Text("On the Node, run `dn node pair`. It shows a link that works for 10 minutes.")
            .font(.footnote)
            .foregroundStyle(.secondary)
        errorText
        Button {
            connect()
        } label: {
            Text("Connect").frame(maxWidth: .infinity, minHeight: 32)
        }
        .buttonStyle(.borderedProminent)
        .disabled(link.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }

    @ViewBuilder private var errorText: some View {
        if let error {
            Label(error, systemImage: "exclamationmark.octagon")
                .foregroundStyle(Color.dnDanger)
                .accessibilityAddTraits(.isStaticText)
        }
    }

    // MARK: Connecting and the numbers

    private func connecting(_ text: String) -> some View {
        HStack(spacing: 8) {
            ProgressView()
            Text(text)
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private func check(_ numbers: [String]) -> some View {
        Text(numbers.joined(separator: " "))
            .font(.system(.title, design: .monospaced))
            .tracking(2)
            .frame(maxWidth: .infinity)
            .padding()
            .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
            .accessibilityLabel(numbers.joined(separator: ", "))
        Text("Your Node shows the same numbers. If they match, confirm on the Node. If not, stop: someone may be between this phone and your Node.")
        HStack(spacing: 8) {
            ProgressView()
            Text("Waiting for you to confirm on the Node…").foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
        Button {
            mismatch()
        } label: {
            Text("They don't match").frame(maxWidth: .infinity, minHeight: 32)
        }
        .buttonStyle(.bordered)
        Button("Cancel") { stop() }
            .frame(maxWidth: .infinity, minHeight: 44)
    }

    // MARK: Actions

    private func connect() {
        guard let phone = model.phone else { return }
        let text = link.trimmingCharacters(in: .whitespacesAndNewlines)
        error = nil
        let back: Step = step == .scan ? .scan : .paste
        let preview: LinkPreview
        do {
            preview = try readLink(text: text)
        } catch {
            self.error = message(error)
            return
        }
        step = .connecting(
            preview.address.map { "Connecting to \(Format.host($0))…" } ?? "Connecting through your Relay…"
        )
        attempt += 1
        let mine = attempt
        let name = model.phoneName
        Task {
            do {
                let numbers = try await phone.startPairing(linkText: text, phoneName: name)
                guard mine == attempt else { return }
                step = .numbers(numbers)
                // No time limit: the person is comparing the numbers on the Node.
                let node = try await phone.finishPairing()
                guard mine == attempt else { return }
                model.phoneName = name
                model.paired(with: node)
                dismiss()
            } catch {
                guard mine == attempt else { return }
                self.error = message(error)
                step = back
            }
        }
    }

    private func mismatch() {
        attempt += 1
        let phone = model.phone
        Task { await phone?.cancelPairing() }
        link = ""
        error = "Pairing stopped. Nothing was shared. Start again with a new link."
        step = .paste
    }

    private func stop() {
        attempt += 1
        if case .scan = step {} else if case .paste = step {} else {
            let phone = model.phone
            Task { await phone?.cancelPairing() }
        }
        dismiss()
    }
}

/// The camera, reading QR codes (VisionKit). Calls `found` with each code's text, and
/// `failed` when the camera can't start.
struct Scanner: UIViewControllerRepresentable {
    let found: (String) -> Void
    let failed: (String) -> Void

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            qualityLevel: .balanced,
            isHighlightingEnabled: true
        )
        scanner.delegate = context.coordinator
        do {
            try scanner.startScanning()
        } catch {
            let reason = error.localizedDescription
            DispatchQueue.main.async { failed(reason) }
        }
        return scanner
    }

    func updateUIViewController(_ scanner: DataScannerViewController, context: Context) {
        context.coordinator.found = found
    }

    func makeCoordinator() -> Coordinator { Coordinator(found: found) }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        var found: (String) -> Void
        init(found: @escaping (String) -> Void) { self.found = found }

        func dataScanner(_ scanner: DataScannerViewController, didAdd items: [RecognizedItem], allItems: [RecognizedItem]) {
            for case .barcode(let code) in items {
                if let text = code.payloadStringValue { found(text) }
            }
        }
    }
}
