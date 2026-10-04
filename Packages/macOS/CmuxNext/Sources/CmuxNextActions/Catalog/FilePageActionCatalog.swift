// The markdown and code editor pages' commands (diff-host S6, S7). The pages read no chord: the
// one key dispatcher runs these while the page has the keyboard (`markdownFocused`, and
// `filePreviewFocused` for the editor) and the app sends the page command. The editor also takes
// the shared find actions and `saveFilePreview` / `toggleFileEditorWordWrap`.

nonisolated enum FilePageActionCatalog: ActionCatalogGroup {
    static func descriptors() -> [ActionDescriptor] {
        [
            ActionDescriptor(
                id: "markdownSave",
                title: String(localized: "action.markdownSave", defaultValue: "Markdown: Save", bundle: .module),
                keywords: ["markdown", "save", "file"], defaultShortcut: Shortcut("s", modifiers: [.command]),
                category: .browser, symbol: "square.and.arrow.down", surfaces: [.keyboard], requires: [.markdownFocused],
                targets: [.pane], cliName: "browser markdown-save"
            ),
            ActionDescriptor(
                id: "markdownBack",
                title: String(localized: "action.markdownBack", defaultValue: "Markdown: Back", bundle: .module),
                keywords: ["markdown", "history", "link"], defaultShortcut: Shortcut("[", modifiers: [.command]),
                category: .browser, symbol: "chevron.backward", surfaces: [.keyboard], requires: [.markdownFocused],
                targets: [.pane], cliName: "browser markdown-back"
            ),
            ActionDescriptor(
                id: "markdownForward",
                title: String(localized: "action.markdownForward", defaultValue: "Markdown: Forward", bundle: .module),
                keywords: ["markdown", "history", "link"], defaultShortcut: Shortcut("]", modifiers: [.command]),
                category: .browser, symbol: "chevron.forward", surfaces: [.keyboard], requires: [.markdownFocused],
                targets: [.pane], cliName: "browser markdown-forward"
            ),
            ActionDescriptor(
                id: "markdownLink",
                title: String(localized: "action.markdownLink", defaultValue: "Markdown: Edit Link", bundle: .module),
                keywords: ["markdown", "link", "url"], defaultShortcut: Shortcut("k", modifiers: [.command]),
                category: .browser, symbol: "link", surfaces: [.keyboard], requires: [.markdownFocused],
                targets: [.pane], cliName: "browser markdown-link"
            ),
            ActionDescriptor(
                id: "fileEditorGotoLine",
                title: String(localized: "action.fileEditorGotoLine", defaultValue: "Editor: Go to Line…", bundle: .module),
                keywords: ["editor", "line", "jump"], defaultShortcut: Shortcut("g", modifiers: [.control]),
                category: .browser, symbol: "arrow.right.to.line", surfaces: [.keyboard], requires: [.filePreviewFocused],
                targets: [.pane], cliName: "browser editor-go-to-line"
            ),
            ActionDescriptor(
                id: "fileEditorReplace",
                title: String(localized: "action.fileEditorReplace", defaultValue: "Editor: Replace…", bundle: .module),
                keywords: ["editor", "find", "replace"], category: .browser, symbol: "arrow.left.arrow.right",
                surfaces: [.keyboard], requires: [.filePreviewFocused], targets: [.pane], cliName: "browser editor-replace"
            ),
            ActionDescriptor(
                id: "fileEditorZoomIn",
                title: String(localized: "action.fileEditorZoomIn", defaultValue: "Editor: Zoom In", bundle: .module),
                keywords: ["editor", "zoom", "font"], defaultShortcut: Shortcut("=", modifiers: [.command]),
                category: .browser, symbol: "plus.magnifyingglass", surfaces: [.keyboard], requires: [.filePreviewFocused],
                targets: [.pane], cliName: "browser editor-zoom-in"
            ),
            ActionDescriptor(
                id: "fileEditorZoomOut",
                title: String(localized: "action.fileEditorZoomOut", defaultValue: "Editor: Zoom Out", bundle: .module),
                keywords: ["editor", "zoom", "font"], defaultShortcut: Shortcut("-", modifiers: [.command]),
                category: .browser, symbol: "minus.magnifyingglass", surfaces: [.keyboard], requires: [.filePreviewFocused],
                targets: [.pane], cliName: "browser editor-zoom-out"
            ),
            ActionDescriptor(
                id: "fileEditorZoomReset",
                title: String(localized: "action.fileEditorZoomReset", defaultValue: "Editor: Actual Size", bundle: .module),
                keywords: ["editor", "zoom", "reset"], defaultShortcut: Shortcut("0", modifiers: [.command]),
                category: .browser, symbol: "1.magnifyingglass", surfaces: [.keyboard], requires: [.filePreviewFocused],
                targets: [.pane], cliName: "browser editor-actual-size"
            ),
            // Any Monaco action (webviews/src/pages/editor/keys.ts EDITOR_ACTIONS) for a user binding.
            ActionDescriptor(
                id: "fileEditorAction",
                title: String(localized: "action.fileEditorAction", defaultValue: "Editor: Run Editor Action", bundle: .module),
                keywords: ["editor", "monaco", "command"], category: .browser, symbol: "command",
                surfaces: [.keyboard], requires: [.filePreviewFocused], arguments: [CatalogArgument.textString],
                targets: [.pane], cliName: "browser editor-action"
            ),
        ]
    }
}
