import SwiftUI

/// 1 · Welcome (design: PhoneCompanion), shown while no Node is paired.
struct WelcomeView: View {
    @Environment(AppModel.self) private var model
    @State private var pairing = false

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 16) {
                Text("See and answer what your Almend Node is waiting for. Your chats stay on your Node and your computer.")
                    .font(.body)
                Spacer()
                Button {
                    pairing = true
                } label: {
                    Text("Pair with your Node").frame(maxWidth: .infinity, minHeight: 32)
                }
                .buttonStyle(.borderedProminent)
                Text("On the Node, run `dn node pair`, or choose Pair another device in Settings › Node on your computer.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .padding()
            .navigationTitle("Almend")
            #if DEBUG
            .onAppear {
                if DemoPhone.screen == .pair || ProcessInfo.processInfo.arguments.contains("-DemiosPairLink") {
                    pairing = true
                }
            }
            #endif
            .sheet(isPresented: $pairing) {
                PairView()
            }
            .alert("This phone is unpaired", isPresented: Bindable(model).nodeStillListsPhone) {
                Button("Done", role: .cancel) {}
            } message: {
                Text("Your Node didn't answer, so it still lists this phone. Run `dn node remove-device` on the Node, or remove it from another paired device.")
            }
        }
    }
}
