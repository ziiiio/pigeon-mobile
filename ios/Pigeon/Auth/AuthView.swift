// AuthView (M5.4) — the homeserver/username/password form, the iOS mirror of
// Android's `AuthScreen`. Renders the `AuthViewModel` state: a spinner while
// restoring/submitting, and any error while signed out. No protocol logic.
import SwiftUI
import PigeonCore

struct AuthView: View {
    let state: AuthState
    let onLogin: (String, String, String) -> Void
    let onRegister: (String, String, String) -> Void

    @State private var server = "https://"
    @State private var username = ""
    @State private var password = ""

    private var isBusy: Bool {
        switch state {
        case .submitting, .restoring: return true
        default: return false
        }
    }

    private var errorMessage: String? {
        if case let .signedOut(error) = state { return error }
        return nil
    }

    private var canSubmit: Bool {
        !isBusy && !server.trimmed.isEmpty && !username.trimmed.isEmpty && !password.isEmpty
    }

    var body: some View {
        VStack(spacing: 20) {
            Spacer()
            VStack(spacing: 8) {
                Image(systemName: "bird.fill").font(.system(size: 44))
                Text("Pigeon").font(.largeTitle.bold())
                Text("Federated, end-to-end encrypted.")
                    .font(.footnote).foregroundStyle(.secondary)
            }

            VStack(spacing: 12) {
                TextField("Homeserver (https://pigeon.example)", text: $server)
                    .textContentType(.URL)
                    .keyboardType(.URL)
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
                TextField("Username", text: $username)
                    .textContentType(.username)
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
                SecureField("Password", text: $password)
                    .textContentType(.password)
            }
            .textFieldStyle(.roundedBorder)

            if let errorMessage {
                Text(errorMessage)
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }

            VStack(spacing: 10) {
                Button {
                    onLogin(server, username, password)
                } label: {
                    HStack {
                        if isBusy { ProgressView().controlSize(.small) }
                        Text("Sign in")
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .disabled(!canSubmit)

                Button("Create account") {
                    onRegister(server, username, password)
                }
                .disabled(!canSubmit)
            }
            Spacer()
        }
        .padding()
        .disabled(isBusy)
    }
}
