// RoomListView (M5.4) — the signed-in room list, the iOS mirror of Android's
// `RoomListScreen`/`RoomListRoute`. It owns the sync loop for its lifetime via
// `SyncController`: the loop runs while this screen is present AND the app is
// active, and the in-flight `/sync` is cancelled on background or teardown
// (Gotcha #6 — the iOS side even folds in `scenePhase`, which Android handles
// separately). The observer bridges the core's change-stream to the view-model.
import SwiftUI
import PigeonCore

struct RoomListView: View {
    let session: Session
    let client: PigeonClient
    let signingOut: Bool
    let signOutError: String?
    let onSignOut: () -> Void

    @StateObject private var vm: RoomsViewModel
    @StateObject private var timeline = TimelineSignal()
    @State private var sync = SyncController()
    @Environment(\.scenePhase) private var scenePhase

    // Sheet/dialog state.
    @State private var showingCreate = false
    @State private var showingJoin = false
    @State private var showingRestore = false

    init(session: Session, client: PigeonClient, signingOut: Bool,
         signOutError: String?, onSignOut: @escaping () -> Void) {
        self.session = session
        self.client = client
        self.signingOut = signingOut
        self.signOutError = signOutError
        self.onSignOut = onSignOut
        _vm = StateObject(wrappedValue: RoomsViewModel(client: client))
    }

    var body: some View {
        NavigationView {
            content
                .navigationTitle("Rooms")
                .toolbar {
                    ToolbarItem(placement: .navigationBarLeading) { signOutButton }
                    ToolbarItem(placement: .navigationBarTrailing) { actionsMenu }
                }
        }
        .navigationViewStyle(.stack)
        .task { await startSync() }
        .onChange(of: scenePhase) { phase in sync.setActive(phase == .active) }
        .onDisappear { sync.setClient(nil, observer: nil) }
        .sheet(isPresented: $showingCreate) {
            CreateRoomSheet { name, topic, encrypted in
                vm.createRoom(name: name, topic: topic, encrypted: encrypted)
            }
        }
        .sheet(isPresented: $showingJoin) {
            TextEntrySheet(title: "Join room", field: "Room id (!room:server)",
                           confirm: "Join") { vm.joinRoom($0) }
        }
        .sheet(isPresented: $showingRestore) {
            TextEntrySheet(title: "Restore keys", field: "Recovery key",
                           confirm: "Restore") { vm.restoreBackup($0) }
        }
        .alert("Save your recovery key",
               isPresented: Binding(get: { vm.state.recoveryKey != nil },
                                    set: { if !$0 { vm.clearRecoveryKey() } })) {
            Button("Done") { vm.clearRecoveryKey() }
        } message: {
            Text(recoveryMessage)
        }
        .alert("Error",
               isPresented: Binding(get: { vm.state.actionError != nil },
                                    set: { if !$0 { vm.clearError() } })) {
            Button("OK") { vm.clearError() }
        } message: {
            Text(vm.state.actionError ?? "")
        }
    }

    @ViewBuilder private var content: some View {
        VStack(spacing: 0) {
            if !vm.state.connected {
                Text("Offline — showing stored messages")
                    .font(.caption).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity).padding(6)
                    .background(Color.yellow.opacity(0.2))
            }
            if let signOutError {
                Text(signOutError).font(.caption).foregroundStyle(.red)
                    .frame(maxWidth: .infinity).padding(6)
            }
            if vm.state.loading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if vm.state.rooms.isEmpty {
                emptyState
            } else {
                roomList
            }
        }
    }

    private var roomList: some View {
        List(vm.state.rooms, id: \.roomId) { room in
            NavigationLink {
                ChatView(client: client, room: room, myUserId: session.userId, timeline: timeline)
            } label: {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(room.name ?? room.roomId).font(.body)
                        if room.name != nil {
                            Text(room.roomId).font(.caption2).foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                    }
                    Spacer()
                    if room.encrypted {
                        Image(systemName: "lock.fill").font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
        .listStyle(.plain)
    }

    private var emptyState: some View {
        VStack(spacing: 10) {
            Image(systemName: "tray").font(.largeTitle).foregroundStyle(.secondary)
            Text("No rooms yet").font(.headline)
            Text("Create one or join with a room id.")
                .font(.footnote).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var signOutButton: some View {
        Button(role: .destructive) { onSignOut() } label: {
            if signingOut { ProgressView().controlSize(.small) } else { Text("Sign out") }
        }
        .disabled(signingOut)
    }

    private var actionsMenu: some View {
        Menu {
            Button { showingCreate = true } label: { Label("New room", systemImage: "plus.bubble") }
            Button { showingJoin = true } label: { Label("Join room", systemImage: "arrow.right.circle") }
            Divider()
            Button { vm.backup() } label: { Label("Back up keys", systemImage: "key") }
            Button { showingRestore = true } label: { Label("Restore keys", systemImage: "arrow.clockwise") }
        } label: {
            Image(systemName: "plus")
        }
    }

    private var recoveryMessage: String {
        let key = vm.state.recoveryKey ?? ""
        return "This is the only secret that can restore your encrypted messages on a "
            + "new device. Save it somewhere safe — it never leaves your device.\n\n\(key)"
    }

    /// Start the core sync loop, bridging its signals into the VM + timeline tick.
    private func startSync() async {
        let observer = RoomSyncObserver(
            onChange: { [weak vm, weak timeline] in
                Task { @MainActor in vm?.reload(); timeline?.bump() }
            },
            onStatus: { [weak vm] connected in
                Task { @MainActor in vm?.setConnected(connected) }
            })
        sync.setClient(client, observer: observer, onEnded: { [weak vm] err in
            Task { @MainActor in vm?.onSyncFailed(authErrorMessage(err)) }
        })
        sync.setActive(scenePhase == .active)
    }
}

// MARK: - Sheets

/// New-room sheet: optional name/topic + an "encrypted" toggle (M3 — the core
/// hosts the MLS group; the UI is otherwise identical).
private struct CreateRoomSheet: View {
    let onCreate: (String?, String?, Bool) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var topic = ""
    @State private var encrypted = true

    var body: some View {
        NavigationView {
            Form {
                TextField("Name (optional)", text: $name)
                TextField("Topic (optional)", text: $topic)
                Toggle("End-to-end encrypted", isOn: $encrypted)
            }
            .navigationTitle("New room")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") { onCreate(name, topic, encrypted); dismiss() }
                }
            }
        }
    }
}

/// A one-field sheet reused for Join (room id) and Restore (recovery key).
private struct TextEntrySheet: View {
    let title: String
    let field: String
    let confirm: String
    let onConfirm: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""

    var body: some View {
        NavigationView {
            Form {
                TextField(field, text: $text)
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
            }
            .navigationTitle(title)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button(confirm) { onConfirm(text); dismiss() }
                        .disabled(text.trimmed.isEmpty)
                }
            }
        }
    }
}
