// ChatView (M5.4) — a room's timeline + composer, the iOS mirror of Android's
// `ChatScreen`. It renders pre-formatted `TimelineEvent`s from the core (bodies,
// system lines, images), pages backward on scroll, sends text/images, and
// refreshes whenever the sync loop ticks the shared `TimelineSignal`. No protocol
// or crypto logic: encryption/decryption + upload/download all live in the core.
import SwiftUI
import PigeonCore

struct ChatView: View {
    let client: PigeonClient
    let room: Room
    let myUserId: String
    @ObservedObject var timeline: TimelineSignal

    @StateObject private var vm: ChatViewModel
    @State private var draft = ""
    @State private var showingInvite = false

    init(client: PigeonClient, room: Room, myUserId: String, timeline: TimelineSignal) {
        self.client = client
        self.room = room
        self.myUserId = myUserId
        self.timeline = timeline
        _vm = StateObject(wrappedValue: ChatViewModel(client: client, roomId: room.roomId))
    }

    var body: some View {
        VStack(spacing: 0) {
            timelineList
            composer
        }
        // A lock prefix marks an end-to-end-encrypted room (M3.6), mirroring
        // Android's chat title. Encryption itself is transparent (handled in core).
        .navigationTitle((room.encrypted ? "🔒 " : "") + (room.name ?? room.roomId))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .navigationBarTrailing) {
                Button { showingInvite = true } label: { Image(systemName: "person.badge.plus") }
            }
        }
        // Refresh on every sync tick — the same signal that refreshes the list.
        .onChange(of: timeline.tick) { _ in vm.refresh() }
        .sheet(isPresented: $showingInvite) {
            InviteSheet { vm.invite($0) }
        }
    }

    private var timelineList: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 6) {
                    if !vm.state.atTop && !vm.state.events.isEmpty {
                        ProgressView().onAppear { vm.loadOlder() }
                    }
                    ForEach(vm.state.events, id: \.eventId) { event in
                        row(event).id(event.eventId)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
            }
            .onChange(of: vm.state.events.count) { _ in
                if let last = vm.state.events.last { withAnimation { proxy.scrollTo(last.eventId, anchor: .bottom) } }
            }
        }
    }

    @ViewBuilder private func row(_ event: TimelineEvent) -> some View {
        if let system = event.systemText {
            Text(system)
                .font(.caption).foregroundStyle(.secondary)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 2)
        } else {
            MessageBubble(event: event, isMine: event.sender == myUserId, loadImage: vm.downloadImage)
        }
    }

    private var composer: some View {
        HStack(spacing: 8) {
            if #available(iOS 16.0, *) {
                PhotoPicker(label: "") { data, mime in
                    let (w, h) = Self.pixelSize(of: data)
                    vm.sendImage(bytes: data, mimetype: mime, width: w, height: h)
                }
            }
            TextField("Message", text: $draft)
                .textFieldStyle(.roundedBorder)
            Button {
                vm.send(draft); draft = ""
            } label: {
                Image(systemName: "arrow.up.circle.fill").font(.title2)
            }
            .disabled(draft.trimmed.isEmpty)
        }
        .padding(8)
    }

    /// Pixel dimensions of image data (for the `p.image` metadata the core sends).
    private static func pixelSize(of data: Data) -> (UInt32, UInt32) {
        guard let img = UIImage(data: data) else { return (0, 0) }
        let w = img.size.width * img.scale, h = img.size.height * img.scale
        return (UInt32(max(0, w)), UInt32(max(0, h)))
    }
}

/// One message bubble: sender (for others), body, an inline image if present, and
/// pending/failed status for local echoes.
private struct MessageBubble: View {
    let event: TimelineEvent
    let isMine: Bool
    let loadImage: (ImageContent) async -> Data?

    var body: some View {
        HStack {
            if isMine { Spacer(minLength: 40) }
            VStack(alignment: isMine ? .trailing : .leading, spacing: 3) {
                if !isMine {
                    Text(event.sender).font(.caption2).foregroundStyle(.secondary)
                }
                if let image = event.image {
                    TimelineImage(image: image, load: loadImage)
                }
                if let body = event.body, !body.isEmpty {
                    Text(body)
                }
                if event.pending {
                    Text("Sending…").font(.caption2).foregroundStyle(.secondary)
                } else if event.failed {
                    Text("Failed to send").font(.caption2).foregroundStyle(.red)
                } else if let time = Self.timeLabel(event.originServerTs) {
                    // A small muted timestamp (M4.5), display-only formatting of the
                    // event's origin_server_ts — no protocol logic. Mirrors Android's
                    // TimeLabel (24h, local zone). Hidden while pending/failed, when
                    // the status hint takes its place.
                    Text(time).font(.caption2).foregroundStyle(.secondary)
                }
            }
            .padding(8)
            .background(isMine ? Color.accentColor.opacity(0.15) : Color.secondary.opacity(0.12))
            .clipShape(RoundedRectangle(cornerRadius: 12))
            if !isMine { Spacer(minLength: 40) }
        }
    }

    /// Local "HH:mm" for a wall-clock millis value, or nil when absent (`<= 0`),
    /// matching Android's `TimeLabel`. Display-only (never an ordering key).
    private static let clock: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm"
        return f
    }()
    private static func timeLabel(_ originServerTs: Int64) -> String? {
        guard originServerTs > 0 else { return nil }
        return clock.string(from: Date(timeIntervalSince1970: Double(originServerTs) / 1000))
    }
}

/// Loads an image's bytes through the core (which decrypts encrypted images —
/// the key never leaves it) and renders them inline.
private struct TimelineImage: View {
    let image: ImageContent
    let load: (ImageContent) async -> Data?
    @State private var uiImage: UIImage?

    var body: some View {
        Group {
            if let uiImage {
                Image(uiImage: uiImage)
                    .resizable().scaledToFit()
                    .frame(maxWidth: 220, maxHeight: 220)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                    .accessibilityLabel("Image")  // M4.5 a11y; mirrors Android's contentDescription
            } else {
                RoundedRectangle(cornerRadius: 8)
                    .fill(Color.secondary.opacity(0.15))
                    .frame(width: 180, height: 140)
                    .overlay(ProgressView())
            }
        }
        .task(id: image.uri) {
            if let data = await load(image), let img = UIImage(data: data) { uiImage = img }
        }
    }
}

/// Invite-a-user sheet (M2.6).
private struct InviteSheet: View {
    let onInvite: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var userId = ""

    var body: some View {
        NavigationView {
            Form {
                TextField("User id (@user:server)", text: $userId)
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
            }
            .navigationTitle("Invite")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Invite") { onInvite(userId); dismiss() }
                        .disabled(userId.trimmed.isEmpty)
                }
            }
        }
    }
}
