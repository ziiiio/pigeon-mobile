// PhotoPicker (M5.3) — the iOS analogue of Android's `PickVisualMedia` photo
// picker, used by the chat screen (M5.4) to attach an image.
//
// OS integration only: it surfaces the system photo picker and returns the
// selected image's raw bytes to the caller. It does NOT touch protocol or
// crypto — encryption + upload is the core's job (`uploadImage`/`encrypt_media`,
// M4.1/M4.2). The bytes go straight across the FFI to the core; the app never
// parses, encrypts, or reasons about them (CLAUDE.md Cardinal Rule).
import SwiftUI
import PhotosUI

/// A button that presents the system photo picker and hands the picked image's
/// bytes (+ MIME type) to `onPick`. Uses `PhotosPicker`, so no photo-library
/// permission prompt is required for a single user-driven selection.
///
/// `PhotosPicker` is iOS 16+; the app's floor is iOS 15 (matching the core's
/// `Package.swift`). The M5.4 chat screen gates the attach affordance behind the
/// same availability, so on iOS 15 the app simply omits image attachment rather
/// than failing to build.
@available(iOS 16.0, *)
struct PhotoPicker: View {
    /// Called with the picked image's raw bytes and best-effort MIME type once a
    /// selection resolves. Not called if the user cancels or loading fails.
    var onPick: (Data, String) -> Void
    var label: String = "Photo"

    @State private var selection: PhotosPickerItem?

    var body: some View {
        PhotosPicker(selection: $selection, matching: .images, photoLibrary: .shared()) {
            Label(label, systemImage: "photo")
        }
        // Single-parameter `onChange` — the two-parameter (old, new) form is
        // iOS 17+, and this view supports iOS 16.
        .onChange(of: selection) { newValue in
            guard let item = newValue else { return }
            Task { await load(item) }
        }
    }

    private func load(_ item: PhotosPickerItem) async {
        // `loadTransferable(Data)` gives the raw file bytes; derive the MIME from
        // the item's declared UTIs (default to JPEG, the common camera-roll type).
        guard let data = try? await item.loadTransferable(type: Data.self) else { return }
        let mime = item.supportedContentTypes.first?.preferredMIMEType ?? "image/jpeg"
        await MainActor.run {
            onPick(data, mime)
            selection = nil
        }
    }
}
