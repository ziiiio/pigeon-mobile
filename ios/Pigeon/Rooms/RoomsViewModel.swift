// RoomsViewModel (M5.4) — the thin view-model over the core's room API, the iOS
// mirror of Android's `RoomsViewModel.kt`. No protocol/crypto logic: it reads the
// room list from the local store (offline-first), drives create/join/backup, and
// folds the sync loop's signals into UI state. The sync loop itself is owned by
// the room-list view so its lifecycle bounds it (Gotcha #6).
import SwiftUI
import PigeonCore

/// UI state for the room list.
struct RoomsState {
    var rooms: [Room] = []
    /// Connectivity to the homeserver, from the sync loop's `onStatus`.
    var connected = true
    /// True until the first room-list read completes.
    var loading = true
    /// A transient action failure (create/join/reload) to surface then dismiss.
    var actionError: String?
    /// After a successful key backup, the recovery key to show the user once
    /// (M4.3) — the only secret that can restore the backup; it never reaches the
    /// server. Cleared when the sheet dismisses.
    var recoveryKey: String?
}

@MainActor
final class RoomsViewModel: ObservableObject {
    @Published private(set) var state = RoomsState()
    private let client: PigeonClient

    init(client: PigeonClient) {
        self.client = client
        reload()
    }

    /// Re-read the room list from the local store (no network).
    func reload() {
        do {
            state.rooms = try client.listRooms()
            state.loading = false
        } catch {
            state.loading = false
            state.actionError = message(error)
        }
    }

    /// Fold the sync loop's connectivity signal into state.
    func setConnected(_ connected: Bool) { state.connected = connected }

    /// The sync loop ended fatally (e.g. the token was revoked).
    func onSyncFailed(_ message: String?) {
        state.connected = false
        state.actionError = message
    }

    /// Create a room; its state arrives via the sync loop (which reloads the
    /// list on change). When `encrypted`, the core creates it E2EE — the UI is
    /// otherwise identical (encryption is transparent, M3).
    func createRoom(name: String?, topic: String?, encrypted: Bool) {
        let n = name?.blankAsNil, t = topic?.blankAsNil
        Task {
            do {
                _ = encrypted ? try await client.createEncryptedRoom(name: n, topic: t)
                              : try await client.createRoom(name: n, topic: t)
            } catch { state.actionError = message(error) }
        }
    }

    /// Join a room by id; membership + timeline arrive on the next sync.
    func joinRoom(_ roomId: String) {
        Task {
            do { try await client.joinRoom(roomId: roomId.trimmed) }
            catch { state.actionError = message(error) }
        }
    }

    /// Back up the device's encryption keys (M4.3). The returned recovery key is
    /// shown once for the user to save; it never reaches the server (Gotcha #1).
    func backup() {
        Task {
            do { state.recoveryKey = try await client.backup() }
            catch { state.actionError = message(error) }
        }
    }

    /// Restore the device's encryption keys from the server-side backup using the
    /// user's recovery key (M4.3). Recovered groups/identity surface via sync.
    func restoreBackup(_ recoveryKey: String) {
        Task {
            do { try await client.restoreBackup(recoveryKey: recoveryKey.trimmed) }
            catch { state.actionError = message(error) }
        }
    }

    func clearRecoveryKey() { state.recoveryKey = nil }
    func clearError() { state.actionError = nil }

    private func message(_ error: Error) -> String { authErrorMessage(error) }
}

extension String {
    /// nil if blank after trimming; else the trimmed string. Mirrors Android's
    /// `ifBlank { null }` for optional name/topic fields.
    var blankAsNil: String? {
        let t = trimmed
        return t.isEmpty ? nil : t
    }
}
