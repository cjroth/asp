// The seed document written into a brand-new, EMPTY vault. It doubles as a
// guided tour of the app AND a showcase of everything the live editor renders:
// YAML frontmatter, a mermaid diagram, a table, task lists, blockquotes and
// fenced code with syntax highlighting. App.tsx writes this as `README.md` only
// when the freshly-created vault has no files yet (so existing desktop folders
// are never clobbered), then opens it as the first file.
export const WELCOME_MD = `---
title: Welcome to your vault
tags: [welcome, guide]
created: 2026-06-27
---

# Welcome to your vault 👋

This is a **local-first, end-to-end-synced notes vault**. Everything you write
lives on your own device first, then syncs — encrypted in transit — to any other
device or person you choose to share with. No server ever sees your plaintext.

This very file shows off what the editor can render. Edit it, delete it, or keep
it around as a cheat-sheet — it's just a normal note.

## How syncing works

Your edits save automatically and then flow out to peers. Here's the loop:

\`\`\`mermaid
flowchart LR
  A[You type] --> B[Autosave]
  B --> C[Local history log]
  C --> D{Sharing on?}
  D -- Yes --> E[Encrypt & sync to peers]
  D -- No --> F[Stays on this device]
  E --> G[Peers merge changes]
\`\`\`

## Keyboard & feature cheat-sheet

| Action | How | Notes |
| --- | --- | --- |
| Save | Automatic | Every edit autosaves after a short pause |
| New file / folder | The **＋** menu in the sidebar | Or right-click the file tree |
| Multi-select | Click + Shift / Cmd-click | Then drag to move, or Delete to remove |
| Time-travel | Open the **History** tab | Scrub the timeline to any past moment |
| Switch vault | Click the vault name | Top of the sidebar |

## Getting started

- [x] Create or open a vault
- [x] Read this welcome note
- [ ] Write your first real note
- [ ] Share a vault with another device
- [ ] Scrub the History timeline to see time-travel

> **Tip:** Nothing here is precious. Try things — every change is captured in the
> History log, so you can always scrub back and restore an earlier version.

## The basics

### Creating & opening vaults
Use **New Vault** to start fresh, or **Connect Vault** to join one someone shared
with you. On desktop a vault is just a folder on disk; in the browser it's stored
privately in this browser. Switch between vaults from the name at the top of the
sidebar.

### Editing & autosave
Just start typing. Markdown renders live as you write — headings, **bold**,
*italic*, links, lists, and the rich blocks shown above. There's no Save button:
edits are written for you automatically a moment after you stop typing.

### Sharing & sync
Turn on sharing for a vault to get an invite code. Hand that code to another
device (or person) and they can **Connect** to it. Changes merge both ways, and
everything is end-to-end encrypted — only your devices can read the contents.

### History, time-travel & the Log
The **History** tab gives you a timeline. Drag the playhead back in time to view
any file exactly as it was, then *Restore* to bring that version back to the
present. The **Log** tab lists individual events — creates, edits, renames and
deletes — as they happened.

### Multi-select & drag-to-move
In the file tree, Shift-click or Cmd/Ctrl-click to select several files at once.
Drag the selection onto a folder to move everything there, or press Delete to
remove it all. Folders move their whole subtree with them.

### The folder path in the status bar
On desktop, the status bar shows where this vault lives on disk. Click that path
to reveal the folder in your system file manager — handy for backups or opening
files in another tool.

## A taste of code

Fenced code blocks render with syntax highlighting:

\`\`\`tsx
function Welcome({ name }: { name: string }) {
  // Your notes are yours — local first, synced second.
  return <h1>Hello, {name}!</h1>;
}
\`\`\`

\`\`\`bash
# There's nothing to install for your notes — they're already on disk.
ls ~/your-vault
\`\`\`

Happy writing. ✍️
`;
