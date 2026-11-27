# Quillnote changelog

## 4.0.0 (2026-09-15)

### Breaking changes
- **Removed the legacy `.qnx` importer.** Notebooks in the old `.qnx` format can no longer be opened. Convert them with Quillnote 3.4 (File > Export > Markdown folder) **before** upgrading.
- **Sync now uses a new sign-in.** After upgrading, every device must sign in again. Notes are not lost, but they won't sync until you do.
- Minimum system: 8 GB RAM. Older 32-bit systems are no longer supported.
- The plugin API moved from version 1 to version 2. Plugins written for version 1 are disabled until their authors update them.

### New
- Offline-first sync: edits made offline are merged when you reconnect, instead of creating "conflicted copy" notes.
- Tables in notes: insert with `/table`, resize columns by dragging.
- Daily note template can be customised (Settings > Daily note).

### Fixed
- Search no longer misses words with accents (e.g. "café").
- Fixed a crash when pasting images larger than 20 MB.

## 3.4.0 (2026-06-02)

### New
- Export to Markdown folder (File > Export > Markdown folder), including attachments.
- Dark mode now follows the system setting.

### Fixed
- Tags with spaces were sometimes split in two.
- Reminders set for midnight fired an hour early after the clocks changed.

### Deprecated
- The `.qnx` importer will be removed in 4.0. Convert old notebooks now.

## 3.3.1 (2026-03-18)

### Fixed
- Security: links in shared notes could run scripts in the preview pane. Update recommended for everyone.
- Fixed slow start-up with more than 10,000 notes.

## 3.3.0 (2026-02-04)

### New
- Note history: restore any version from the last 30 days.
- Keyboard shortcut to move a note: Ctrl+Shift+M.

### Changed
- Attachments larger than 50 MB are now stored as links instead of inside the notebook.
