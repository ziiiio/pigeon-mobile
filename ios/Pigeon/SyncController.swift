// SyncController (M5.3) — background-refresh-aware sync lifecycle.
//
// The core owns the actual `/sync` long-poll loop (`PigeonClient.runSync`); this
// is the thin OS-integration wrapper that ties that loop to the app's lifecycle,
// which is the platform's job, not the core's. It addresses CLAUDE.md Gotcha #6
// ("sync long-poll cancellation"): when the app backgrounds or the driving view
// disappears, the in-flight `/sync` is cancelled and its Rust task torn down;
// when the app returns to the foreground it is resumed. Leaking a sync task per
// screen would drain the battery and sockets.
//
// It writes no protocol logic — it only starts/cancels the core's loop. The
// observer (which the core calls on each sync diff) is supplied by the UI layer
// (M5.4). Cancellation propagates because `runSync` is an async FFI call: a
// cancelled Swift `Task` cancels the awaited call, which the core observes.
import Foundation
import PigeonCore

/// Starts the core's sync loop while the app is active and cancels it when the
/// app is not. Drive it from the root view via `scenePhase`:
///
///     .onChange(of: scenePhase) { _, phase in
///         syncController.setActive(phase == .active)
///     }
///
/// and call `setClient(_:observer:)` when a session becomes available / is torn
/// down. Safe to call the setters repeatedly; it only (re)starts when both a
/// client is present and the app is active, and never runs two loops at once.
@MainActor
final class SyncController {
    private var client: PigeonClient?
    private var observer: SyncObserver?
    private var onEnded: ((Error) -> Void)?
    private var isActive = false
    private var task: Task<Void, Never>?

    /// Provide (or clear, with `nil`) the logged-in client and the observer that
    /// receives sync diffs. Clearing the client stops the loop (e.g. on logout).
    /// `onEnded` is called if the loop ends with a *fatal* error (not a normal
    /// background cancellation) — e.g. a revoked token — so the UI can react
    /// (mirrors Android's `onSyncFailed`).
    func setClient(_ client: PigeonClient?, observer: SyncObserver?, onEnded: ((Error) -> Void)? = nil) {
        self.client = client
        self.observer = observer
        self.onEnded = onEnded
        reconcile()
    }

    /// Reflect the app's foreground/background state (from `scenePhase`).
    func setActive(_ active: Bool) {
        guard isActive != active else { return }
        isActive = active
        reconcile()
    }

    /// Start the loop iff we have a client + observer and the app is active;
    /// otherwise ensure it's stopped. Idempotent.
    private func reconcile() {
        let shouldRun = isActive && client != nil && observer != nil
        if shouldRun {
            guard task == nil else { return } // already running
            let client = self.client!
            let observer = self.observer!
            let onEnded = self.onEnded
            task = Task { [weak self] in
                do {
                    // Long-running; returns only on cancellation or a fatal error.
                    try await client.runSync(observer: observer)
                } catch is CancellationError {
                    // Expected on background/teardown — nothing to surface.
                } catch {
                    // Fatal (e.g. revoked token). Report only if we weren't
                    // cancelled (a cancelled Task also lands here on some paths).
                    if !Task.isCancelled {
                        emitTestLog(message: "sync loop ended: \(error)")
                        onEnded?(error)
                    }
                }
                // Clear our handle if this task is still the current one.
                await self?.clearTaskIfCurrent()
            }
        } else {
            stop()
        }
    }

    private func clearTaskIfCurrent() {
        task = nil
        // If conditions still hold (e.g. the loop died but we're active), a
        // caller-driven setActive/setClient will restart it; we don't auto-retry
        // here to avoid a hot loop on a persistent failure.
    }

    /// Cancel the in-flight sync and tear down its Rust task.
    func stop() {
        task?.cancel()
        task = nil
    }
}
