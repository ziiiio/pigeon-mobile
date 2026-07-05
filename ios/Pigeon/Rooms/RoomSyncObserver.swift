// RoomSyncObserver (M5.4) — bridges the core's sync change-stream back into the
// SwiftUI layer, the iOS mirror of the anonymous `SyncObserver` Android creates
// in `RoomListScreen`. The core calls `onChange`/`onStatus` from its sync task
// (off the main actor), so both handlers hop to the main actor before touching
// view-model state.
import Combine
import PigeonCore

/// A monotonic "the store changed" tick the open chat observes, so a room's
/// timeline refreshes on the same sync events that refresh the list.
@MainActor
final class TimelineSignal: ObservableObject {
    @Published private(set) var tick: Int = 0
    func bump() { tick += 1 }
}

/// Forwards the core's sync signals to the room-list VM and the timeline tick.
/// Handlers are plain closures so the view can wire them to `@StateObject` VMs
/// without this type retaining them beyond the sync loop's lifetime.
final class RoomSyncObserver: SyncObserver {
    private let changeHandler: @Sendable () -> Void
    private let statusHandler: @Sendable (Bool) -> Void

    init(onChange: @escaping @Sendable () -> Void, onStatus: @escaping @Sendable (Bool) -> Void) {
        self.changeHandler = onChange
        self.statusHandler = onStatus
    }

    func onChange() { changeHandler() }
    func onStatus(connected: Bool) { statusHandler(connected) }
}
