//
//  FileBrowserView.swift
//  Dub
//
//  M10.5b slim filesystem browser. Sits in the LIBRARY region of the
//  Performance View and replaces the M10.3 `LibraryPlaceholder` for
//  M10.5b dev-loop testing. **Intentionally below M11** — no library
//  database, no crate concept, no Serato / Traktor import.
//
//  PRD §9.7 (TBD) + M10.5b spec:
//      • Folder picker at the top (NSOpenPanel — directory only).
//      • Listing pane: subfolders first, then audio files.
//      • Single-click selects a row. Selecting a *file* updates
//        `model.browserSelection`, which `Space` then loads into
//        the non-master, stopped deck (PRD §5.5).
//      • Single-click on a folder descends into it.
//      • Drag-and-drop a file from this view onto a deck pane also
//        works (the system pasteboard carries the file URL —
//        Finder-style drag bypasses the browser entirely; we just
//        ensure the row is `.draggable`).
//      • **No double-click load**, per the user's M10.5b decision.
//

import AppKit
import SwiftUI
import UniformTypeIdentifiers

import DubCore

/// Audio file extensions the browser shows. Conservative set —
/// formats `dub-io` is known to decode through `symphonia`. Future
/// formats (Opus, etc.) get added here as they pass smoke-load.
private let kAudioExtensions: Set<String> = [
    "mp3", "m4a", "aac", "wav", "wave", "aif", "aiff", "flac", "ogg",
]

/// One row in the browser. Either a subdirectory or an audio file.
struct BrowserEntry: Identifiable, Hashable {
    let url: URL
    let isDirectory: Bool

    var id: URL { url }

    var displayName: String {
        url.lastPathComponent
    }

    var ext: String {
        url.pathExtension.uppercased()
    }
}

/// Slim browser view bound to the app model. Tracks its own current
/// directory + listing; only the **selection** flows back into the
/// model so `Space` can load it.
struct FileBrowserView: View {

    @ObservedObject var model: WaveformAppModel

    @State private var currentDirectory: URL = FileBrowserView.defaultDirectory()
    @State private var entries: [BrowserEntry] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider().overlay(DubColor.divider)
            listing
        }
        .frame(minHeight: DubLayout.libraryMinHeight)
        .background(DubColor.surface1)
        .onAppear(perform: reload)
    }

    // MARK: - Header (folder path + picker)

    private var header: some View {
        HStack(spacing: DubSpacing.sm) {
            Text("LIBRARY")
                .font(DubFont.caps)
                .tracking(1.2)
                .foregroundStyle(DubColor.textSecondary)
            Divider().frame(height: 12).overlay(DubColor.divider)

            Button {
                goUp()
            } label: {
                Image(systemName: "arrow.up")
            }
            .disabled(currentDirectory.pathComponents.count <= 1)
            .help("Go up one folder")

            Text(currentDirectory.path)
                .font(DubFont.body)
                .foregroundStyle(DubColor.textPrimary)
                .lineLimit(1)
                .truncationMode(.head)

            Spacer(minLength: 0)

            Button("Choose folder…") {
                presentFolderPicker()
            }
            .controlSize(.small)
        }
        .padding(.horizontal, DubSpacing.lg)
        .padding(.vertical, DubSpacing.sm)
        .background(DubColor.surface2)
    }

    // MARK: - Listing

    @ViewBuilder
    private var listing: some View {
        if entries.isEmpty {
            emptyListing
        } else {
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(entries) { entry in
                        row(for: entry)
                    }
                }
                .padding(.vertical, DubSpacing.xs)
            }
        }
    }

    private var emptyListing: some View {
        VStack(spacing: DubSpacing.sm) {
            Spacer()
            Text("No audio files in this folder")
                .font(DubFont.body)
                .foregroundStyle(DubColor.textTertiary)
            Text("Use “Choose folder…” to navigate to your music.")
                .font(DubFont.micro)
                .foregroundStyle(DubColor.textPlaceholder)
            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder
    private func row(for entry: BrowserEntry) -> some View {
        let isSelected = (model.browserSelection == entry.url) && !entry.isDirectory
        Button {
            if entry.isDirectory {
                currentDirectory = entry.url
                reload()
            } else {
                model.browserSelection = entry.url
            }
        } label: {
            HStack(spacing: DubSpacing.md) {
                Image(systemName: entry.isDirectory ? "folder" : "waveform")
                    .foregroundStyle(
                        entry.isDirectory
                            ? DubColor.textSecondary
                            : DubColor.textPrimary
                    )
                Text(entry.displayName)
                    .font(DubFont.body)
                    .foregroundStyle(DubColor.textPrimary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 0)
                if !entry.isDirectory {
                    Text(entry.ext)
                        .font(DubFont.micro)
                        .foregroundStyle(DubColor.textTertiary)
                }
            }
            .padding(.horizontal, DubSpacing.lg)
            .padding(.vertical, DubSpacing.xs)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                isSelected ? DubColor.surface2 : Color.clear
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .if(!entry.isDirectory) { view in
            // Make audio rows Finder-draggable so the user can drop
            // them onto a deck pane. Drag carries the file URL on
            // the pasteboard via the system promise.
            view.draggable(entry.url) {
                Text(entry.displayName)
                    .font(DubFont.body)
                    .padding(DubSpacing.sm)
                    .background(DubColor.surface2)
                    .clipShape(RoundedRectangle(cornerRadius: 4))
            }
        }
    }

    // MARK: - Filesystem helpers

    private func reload() {
        entries = Self.listing(at: currentDirectory)
    }

    private func goUp() {
        let parent = currentDirectory.deletingLastPathComponent()
        if parent != currentDirectory {
            currentDirectory = parent
            reload()
        }
    }

    private func presentFolderPicker() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.directoryURL = currentDirectory
        panel.prompt = "Choose"
        if panel.runModal() == .OK, let url = panel.url {
            currentDirectory = url
            reload()
        }
    }

    private static func listing(at dir: URL) -> [BrowserEntry] {
        let fm = FileManager.default
        let opts: FileManager.DirectoryEnumerationOptions =
            [.skipsHiddenFiles, .skipsSubdirectoryDescendants, .skipsPackageDescendants]
        guard
            let items = try? fm.contentsOfDirectory(
                at: dir,
                includingPropertiesForKeys: [.isDirectoryKey],
                options: opts)
        else {
            return []
        }
        let mapped: [BrowserEntry] = items.compactMap { url -> BrowserEntry? in
            let values = try? url.resourceValues(forKeys: [.isDirectoryKey])
            let isDir = values?.isDirectory ?? false
            if isDir {
                return BrowserEntry(url: url, isDirectory: true)
            }
            let ext = url.pathExtension.lowercased()
            if kAudioExtensions.contains(ext) {
                return BrowserEntry(url: url, isDirectory: false)
            }
            return nil
        }
        // Folders first, then files; both sorted case-insensitively.
        return mapped.sorted { (lhs, rhs) in
            if lhs.isDirectory != rhs.isDirectory {
                return lhs.isDirectory && !rhs.isDirectory
            }
            return lhs.displayName.localizedCaseInsensitiveCompare(rhs.displayName)
                == .orderedAscending
        }
    }

    /// Default landing directory: `~/Music` if it exists, otherwise
    /// `~`. Most macOS users keep their library under Music; the
    /// fallback ensures the picker never opens onto a non-existent
    /// path.
    private static func defaultDirectory() -> URL {
        let fm = FileManager.default
        let home = fm.homeDirectoryForCurrentUser
        let music = home.appendingPathComponent("Music")
        var isDir: ObjCBool = false
        if fm.fileExists(atPath: music.path, isDirectory: &isDir), isDir.boolValue {
            return music
        }
        return home
    }
}

// MARK: - Conditional view modifier

private extension View {
    /// Apply `transform` only when `condition` is true. Used for
    /// conditional `.draggable` since SwiftUI's modifier signature
    /// would otherwise force us to apply `.draggable` to folder
    /// rows too.
    @ViewBuilder
    func `if`<Transform: View>(
        _ condition: Bool,
        transform: (Self) -> Transform
    ) -> some View {
        if condition {
            transform(self)
        } else {
            self
        }
    }
}

#Preview {
    FileBrowserView(model: WaveformAppModel())
        .frame(width: 1440, height: 360)
}
