// RootView (M5.3) — the app shell.
//
// M5.3 lands the OS-integration layer (KeychainKeyStore, OsLogSink, PhotoPicker,
// SyncController) and this shell that boots on top of it. The real M1–M4 flow
// screens (auth, room list, chat) are M5.4 and will replace this body, driven by
// the shared core through the same bindings the Android app uses.
//
// For now it proves the app boots with the core callbacks installed and can
// round-trip a value from the core — the on-device signal that the OS-integration
// wiring is live. It deliberately does not build session/room UI (that is M5.4).
import SwiftUI
import PigeonCore

struct RootView: View {
    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "bird.fill")
                .font(.largeTitle)
            Text("Pigeon")
                .font(.headline)
            Text(coreVersion())
                .font(.footnote.monospaced())
                .foregroundStyle(.secondary)
            Text("OS integration ready — sign-in UI lands in M5.4")
                .font(.caption)
                .foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
        }
        .padding()
    }
}

#Preview {
    RootView()
}
