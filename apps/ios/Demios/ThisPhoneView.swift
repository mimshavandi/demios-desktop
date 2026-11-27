import SwiftUI

/// 5 · This phone (design: PhoneCompanion): the Node it's paired with, its name there, and
/// Unpair.
struct ThisPhoneView: View {
    @Environment(AppModel.self) private var model
    @State private var confirming = false
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        List {
            Section {
                LabeledContent("Node", value: model.node?.name ?? model.paired?.name ?? "")
                if let version = model.node?.version {
                    LabeledContent("Version", value: version)
                }
                if let address = model.paired?.address {
                    LabeledContent("Reached through", value: address)
                }
                LabeledContent("Name on the Node", value: model.phoneName)
            }
            Section {
                Button(role: .destructive) {
                    error = nil
                    confirming = true
                } label: {
                    HStack {
                        Text("Unpair this phone")
                        if busy { Spacer(); ProgressView() }
                    }
                }
                .disabled(busy)
            } footer: {
                if let error {
                    Label(error, systemImage: "exclamationmark.octagon").foregroundStyle(Color.dnDanger)
                }
            }
        }
        .navigationTitle("This phone")
        .navigationBarTitleDisplayMode(.inline)
        .alert("Unpair this phone?", isPresented: $confirming) {
            Button("Cancel", role: .cancel) {}
            Button("Unpair", role: .destructive) { unpair() }
        } message: {
            Text("This phone stops seeing what's waiting, and its copy is deleted. Your Node and computer keep everything.")
        }
    }

    private func unpair() {
        busy = true
        Task {
            do {
                try await model.unpair()
            } catch {
                self.error = message(error)
            }
            busy = false
        }
    }
}
