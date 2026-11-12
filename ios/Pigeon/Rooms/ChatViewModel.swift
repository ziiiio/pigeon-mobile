// ChatViewModel (M5.4) — the thin view-model over a room's timeline, the iOS
// mirror of Android's `ChatViewModel.kt`. Reads are offline-first from the local
// store; on open it backfills recent history from the server so the room isn't
// empty before sync has covered it. No protocol logic: bodies and system lines
// are pre-rendered by the core (`TimelineEvent`); this only pages and merges by
// the opaque `cursor`.
import SwiftUI
import PigeonCore

@MainActor
final class ChatViewModel: ObservableObject {
    /// How many timeline events to read per page.
    private static let page: UInt32 = 50

    struct ChatState {
        var loading = true
        /// Oldest-first.
        var events: [TimelineEvent] = []
        /// True once backward pagination reaches the start of stored history.
        var atTop = false
        var error: String?
    }

    @Published private(set) var state = ChatState()
    private let client: PigeonClient
    private let roomId: String

    init(client: PigeonClient, roomId: String) {
        self.client = client
        self.roomId = roomId
        refresh()
        // Top up recent history from the server; the refresh afterwards folds it in.
        Task {
            _ = try? await client.fetchMessages(roomId: roomId, limit: Self.page)
            refresh()
        }
    }

    /// Re-read the newest page and merge it with what's already loaded.
    func refresh() {
        do {
            // Re-read the whole loaded range, not just the newest page, so every
            // row on screen picks up its current state (decrypted / failed /
            // confirmed). Mirrors Android's ChatViewModel.refresh.
            let window = max(Self.page, UInt32(state.events.count))
            let newest = try client.timeline(roomId: roomId, limit: window, before: nil)
            state.loading = false
            state.events = mergeTimeline(existing: state.events, fresh: newest)
        } catch {
            state.loading = false
            state.error = chatErrorMessage(error)
        }
    }

    /// Drop the shown error — when the user dismisses the alert, and at the start
    /// of every user action (send / invite / attach) so an earlier failure doesn't
    /// linger over a later one. Mirrors Android's `ChatViewModel.clearError`.
    func clearError() {
        state.error = nil
    }

    /// Page backwards from the oldest loaded event (scroll-to-load-older).
    func loadOlder() {
        guard !state.atTop, !state.loading, let oldest = state.events.first else { return }
        do {
            let older = try client.timeline(roomId: roomId, limit: Self.page, before: oldest.cursor)
            if older.isEmpty { state.atTop = true }
            else { state.events = mergeTimeline(existing: state.events, fresh: older) }
        } catch {
            state.error = chatErrorMessage(error)
        }
    }

    /// Send a plaintext message. The core writes a local echo and queues it
    /// (offline-first), so the refresh shows it immediately — pending, then
    /// confirmed once the server acks (or failed on rejection).
    func send(_ body: String) {
        let text = body.trimmed
        guard !text.isEmpty else { return }
        clearError()
        Task {
            do { try await client.sendMessage(roomId: roomId, body: text) }
            catch { state.error = chatErrorMessage(error) }
            refresh()
        }
    }

    /// Invite a user (M2.6). Surfaces in the timeline as a system line via sync.
    func invite(_ userId: String) {
        let id = userId.trimmed
        guard !id.isEmpty else { return }
        clearError()
        Task {
            do { try await client.invite(roomId: roomId, userId: id) }
            catch { state.error = chatErrorMessage(error) }
            refresh()
        }
    }

    /// Attach an image (M4.1/M4.2): hand the raw bytes to the core, which uploads
    /// and sends the right message for the room (plaintext `p.image`, or an
    /// encrypted-then-uploaded blob with the per-file key inside the E2EE message).
    /// All crypto/protocol stays in the core.
    func sendImage(bytes: Data, mimetype: String, width: UInt32, height: UInt32) {
        clearError()
        Task {
            do {
                try await client.sendImage(roomId: roomId, bytes: bytes, mimetype: mimetype,
                                           width: width, height: height, caption: "")
            } catch { state.error = chatErrorMessage(error) }
            refresh()
        }
    }

    /// Download an image's displayable bytes for inline rendering (M4.1/M4.2);
    /// the core decrypts encrypted images (the key never leaves it). `nil` on
    /// failure (offline / gone).
    func downloadImage(_ image: ImageContent) async -> Data? {
        try? await client.downloadImage(image: image)
    }

    /// Union two pages by event id and order by the opaque cursor (DAG depth).
}

/// Merge a freshly-read page into what's on screen, by event id, ordered by
/// cursor. **The fresh copy wins**: an event's rendering changes after it first
/// loads — pending decryption (hidden) → decrypted or "unable to decrypt",
/// sending → confirmed — and keeping the stale copy left those rows hidden, e.g.
/// every message after a key restore. Mirrors Android's `mergeTimeline`.
func mergeTimeline(existing: [TimelineEvent], fresh: [TimelineEvent]) -> [TimelineEvent] {
    var seen = Set<String>()
    return (fresh + existing)
        .filter { seen.insert($0.eventId).inserted }
        .sorted { $0.cursor < $1.cursor }
}

/// The user-facing message for a failed chat action (send / invite / attach /
/// timeline read). Branches on the typed `P_` code, never on error text, and
/// never shows the raw error. Reuses `authErrorMessage` except where its sign-in
/// wording would mislead in a chat: a malformed invitee id comes back as
/// `.invalidUsername` ("Usernames may use only…") and a refused action as
/// `.forbidden` ("Incorrect username or password"). Mirrors Android's
/// `chatErrorMessage` — keep the wording in lockstep.
func chatErrorMessage(_ error: Error) -> String {
    if case let .Api(code, _)? = error as? CoreError {
        switch code {
        case .invalidUsername:
            return "That isn't a valid user ID. Use the full form, e.g. @bob:localhost."
        case .forbidden:
            return "You don't have permission to do that in this room."
        default:
            break
        }
    }
    return authErrorMessage(error)
}
