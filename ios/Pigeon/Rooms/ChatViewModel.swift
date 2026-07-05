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
            let newest = try client.timeline(roomId: roomId, limit: Self.page, before: nil)
            state.loading = false
            state.events = merge(state.events, newest)
        } catch {
            state.loading = false
            state.error = authErrorMessage(error)
        }
    }

    /// Page backwards from the oldest loaded event (scroll-to-load-older).
    func loadOlder() {
        guard !state.atTop, !state.loading, let oldest = state.events.first else { return }
        do {
            let older = try client.timeline(roomId: roomId, limit: Self.page, before: oldest.cursor)
            if older.isEmpty { state.atTop = true }
            else { state.events = merge(older, state.events) }
        } catch {
            state.error = authErrorMessage(error)
        }
    }

    /// Send a plaintext message. The core writes a local echo and queues it
    /// (offline-first), so the refresh shows it immediately — pending, then
    /// confirmed once the server acks (or failed on rejection).
    func send(_ body: String) {
        let text = body.trimmed
        guard !text.isEmpty else { return }
        Task {
            do { try await client.sendMessage(roomId: roomId, body: text) }
            catch { state.error = authErrorMessage(error) }
            refresh()
        }
    }

    /// Invite a user (M2.6). Surfaces in the timeline as a system line via sync.
    func invite(_ userId: String) {
        let id = userId.trimmed
        guard !id.isEmpty else { return }
        Task {
            do { try await client.invite(roomId: roomId, userId: id) }
            catch { state.error = authErrorMessage(error) }
            refresh()
        }
    }

    /// Attach an image (M4.1/M4.2): hand the raw bytes to the core, which uploads
    /// and sends the right message for the room (plaintext `p.image`, or an
    /// encrypted-then-uploaded blob with the per-file key inside the E2EE message).
    /// All crypto/protocol stays in the core.
    func sendImage(bytes: Data, mimetype: String, width: UInt32, height: UInt32) {
        Task {
            do {
                try await client.sendImage(roomId: roomId, bytes: bytes, mimetype: mimetype,
                                           width: width, height: height, caption: "")
            } catch { state.error = authErrorMessage(error) }
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
    private func merge(_ a: [TimelineEvent], _ b: [TimelineEvent]) -> [TimelineEvent] {
        var seen = Set<String>()
        return (a + b)
            .filter { seen.insert($0.eventId).inserted }
            .sorted { $0.cursor < $1.cursor }
    }
}
