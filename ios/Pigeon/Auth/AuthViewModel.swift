// AuthViewModel (M5.4) — the thin view-model over the core's session API, the
// iOS mirror of Android's `AuthViewModel.kt`. It owns no protocol/crypto logic:
// it calls the core's async functions, holds the opaque client handle (the token
// stays inside it — Gotcha #1), and publishes UI state.
import SwiftUI
import PigeonCore

/// UI state for the auth flow (mirrors Android's `AuthState`).
enum AuthState {
    /// Checking for a persisted session on launch.
    case restoring
    /// Signed out — showing the form; `error` is the last failure, if any.
    case signedOut(error: String?)
    /// A register/login is in flight.
    case submitting
    /// Signed in. `client` is the opaque core handle the rooms/sync flows hang
    /// off. `signingOut` is true while a logout is in flight; `error` carries a
    /// logout failure that left the session intact (so the user can retry).
    case signedIn(session: Session, client: PigeonClient, signingOut: Bool, error: String?)
}

@MainActor
final class AuthViewModel: ObservableObject {
    @Published private(set) var state: AuthState = .restoring

    // The logged-in client handle; the token stays inside it (in the core). Kept
    // for the flows that hang off it (sync, logout); never unwrapped into
    // app-level secret state.
    private var client: PigeonClient?

    init() {
        Task { await restore() }
    }

    /// Restore a persisted session on launch (offline-first, in the core). A
    /// restore fault must not wedge launch — fall back to the form.
    private func restore() async {
        do {
            if let restored = try await PigeonCore.restoreSession() {
                client = restored
                state = .signedIn(session: restored.session(), client: restored,
                                  signingOut: false, error: nil)
            } else {
                state = .signedOut(error: nil)
            }
        } catch {
            state = .signedOut(error: authErrorMessage(error))
        }
    }

    func login(server: String, username: String, password: String) {
        submit { try await PigeonCore.login(server: server.trimmed, user: username.trimmed, password: password) }
    }

    func register(server: String, username: String, password: String) {
        submit { try await PigeonCore.register(server: server.trimmed, username: username.trimmed, password: password) }
    }

    /// Sign out: revoke the token + clear the persisted session (in the core). On
    /// success drop the handle and return to the form. The core clears local
    /// state even if the server revoke fails; the only error surfaced is a
    /// keystore fault — the session is then still live, so we stay signed in and
    /// show the reason for a retry.
    func logout() {
        guard case let .signedIn(session, _, signingOut, _) = state, !signingOut,
              let c = client else { return }
        state = .signedIn(session: session, client: c, signingOut: true, error: nil)
        Task {
            do {
                try await c.logout()
                client = nil
                state = .signedOut(error: nil)
            } catch {
                state = .signedIn(session: session, client: c, signingOut: false,
                                  error: authErrorMessage(error))
            }
        }
    }

    private func submit(_ call: @escaping () async throws -> PigeonClient) {
        if case .submitting = state { return }
        state = .submitting
        Task {
            do {
                let c = try await call()
                client = c
                state = .signedIn(session: c.session(), client: c, signingOut: false, error: nil)
            } catch {
                state = .signedOut(error: authErrorMessage(error))
            }
        }
    }
}

extension String {
    /// Trim leading/trailing whitespace (server/username fields), matching the
    /// Android VM's `.trim()`.
    var trimmed: String { trimmingCharacters(in: .whitespacesAndNewlines) }
}
