// RootView (M5.4) — the app's routing shell, the iOS mirror of Android's
// `MainActivity.AuthFlow`. It renders `AuthViewModel` state: the sign-in form
// while restoring/submitting/signed-out, and the room list once signed in. The
// core's host callbacks are installed in `PigeonApp`; this view only routes.
import SwiftUI
import PigeonCore

struct RootView: View {
    @StateObject private var auth = AuthViewModel()

    var body: some View {
        switch auth.state {
        case let .signedIn(session, client, signingOut, error):
            RoomListView(session: session, client: client,
                         signingOut: signingOut, signOutError: error,
                         onSignOut: auth.logout)
        case .restoring, .submitting, .signedOut:
            AuthView(state: auth.state, onLogin: auth.login, onRegister: auth.register)
        }
    }
}
