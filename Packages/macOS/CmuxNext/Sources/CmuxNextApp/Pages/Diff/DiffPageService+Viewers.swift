import Foundation

/// R89's diff seam: the viewer open actions (the palette, the picker, the CLI) open diff tabs
/// through ``DiffPageService``. A folder in no repository opens the empty diff tab, as the S4
/// actions did, so the user can pick another folder there.
extension DiffPageService: DiffViewerOpening {
    func openDiff(directory: String, in pane: PaneController, focus: Bool) throws {
        let folder = URL(fileURLWithPath: directory, isDirectory: true)
        // task-owner: one open of the chosen folder; ends when its tab shows or the empty tab opens
        Task { [weak self, weak pane] in
            guard let self, let pane else { return }
            do {
                try await self.open(folder: folder, in: pane, focus: focus)
            } catch {
                self.openEmpty(in: pane, focus: focus)
            }
        }
    }
}
