// CleartextPolicy — refuse an `http://` homeserver this build doesn't allow, before
// the password reaches the core. Mirrors Android's `CleartextPolicy.kt` (same wording,
// same cases).
//
// Why the app checks this itself: App Transport Security governs URLSession, and the
// Rust core's `reqwest` client opens its own sockets, so ATS would not stop a release
// build sending the password and token over `http://`. Release builds permit no
// cleartext; debug builds permit `localhost` (the simulator's pre-filled
// `http://localhost:8008`). Android's equivalent allowlist is its debug
// network-security-config (`10.0.2.2`, `localhost`).
import Foundation

/// Shown when the user enters an `http://` homeserver the build doesn't allow.
let cleartextRefused =
    "This address uses http://, which would send your password and messages unencrypted. "
    + "Use an https:// address."

/// The refusal message, or nil if `server` may be used. An address that isn't a
/// parseable `http://host` URL passes through, for the core to reject as usual.
func cleartextRefusal(_ server: String,
                      isCleartextPermitted: (String) -> Bool = platformPermitsCleartext) -> String? {
    guard let url = URL(string: server.trimmed),
          url.scheme?.lowercased() == "http",
          let host = url.host
    else { return nil }
    return isCleartextPermitted(host) ? nil : cleartextRefused
}

/// This build's cleartext allowlist: `localhost` in debug builds, nothing in release.
func platformPermitsCleartext(_ host: String) -> Bool {
    #if DEBUG
    return host == "localhost"
    #else
    return false
    #endif
}
