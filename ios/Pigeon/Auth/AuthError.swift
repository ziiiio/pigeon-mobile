// AuthError (M5.4) — maps a typed `CoreError` to a user-facing message. The iOS
// mirror of Android's `AuthError.kt`; it must stay in lockstep with it (same
// codes → same wording) so the two apps behave identically.
//
// The UI branches on the typed `P_` code, NEVER on error text (CLAUDE.md), and
// handles every variant — a federated, offline-prone client will hit them all.
// A pure function, unit-tested without a simulator (AuthErrorTests).
import PigeonCore

func authErrorMessage(_ error: Error) -> String {
    guard let core = error as? CoreError else {
        // Non-core errors shouldn't reach the UI from the core's async FFI, but
        // never show a raw Swift error — degrade to a generic message.
        return "Something went wrong."
    }
    switch core {
    case let .Api(code, _):
        switch code {
        case .userInUse: return "That username is already taken."
        case .forbidden: return "Incorrect username or password."
        case .invalidUsername: return "Usernames may use only a–z, 0–9, and . _ -"
        case .unknownToken, .missingToken:
            return "Your session has expired. Please sign in again."
        case .limitExceeded: return "Too many attempts. Please wait a moment and try again."
        case .badJson, .notJson: return "The server rejected the request."
        case .notFound: return "Not found on this server."
        case .badSignature, .unrecognized, .unknown, .other:
            return "The server reported an error."
        }
    case .Network: return "Can't reach the server. Check the address and your connection."
    case .Protocol: return "Unexpected response from the server."
    case .Storage: return "Couldn't access secure storage on this device."
    case .Crypto: return "A security error occurred."
    }
}
