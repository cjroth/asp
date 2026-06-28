# ASP Vault Editor — Design Specification for GPUI Port

A pixel-perfect design specification for reimplementing the React+Tauri ASP Vault Editor in Rust/GPUI (Zed's GUI framework). This spec documents every visual element, color, spacing, typography, interaction state, and component needed for faithful parity.

## 1. Design Tokens

### 1.1 Color Palette

**Light Theme (default, [data-theme="light"] or unset):**
- `--bg: #ffffff` — Main background
- `--bg-sub: #fafaf8` — Secondary background (sidebar, history bar)
- `--bg-input: #faf9f7` — Input/code block background
- `--text: #1c1917` — Primary text
- `--text2: #57534e` — Secondary text
- `--text3: #78716c` — Tertiary text (faint headings, tabs)
- `--faint: #a8a29e` — Very faint text (borders, placeholders)
- `--faint2: #b0aaa2` — Slightly darker faint (labels, disabled)
- `--line: #ededea` — Dividers, borders
- `--overlay: rgba(28, 25, 23, 0.30)` — Modal backdrop overlay

**Dark Theme ([data-theme="dark"]):**
- `--bg: #1b1b1e`
- `--bg-sub: #161618`
- `--bg-input: #232327`
- `--text: #ececec`
- `--text2: #b6b2ab`
- `--text3: #9b968d`
- `--faint: #827d74`
- `--faint2: #6f6a62`
- `--line: #2d2d31`
- `--overlay: rgba(0, 0, 0, 0.62)`

**Scrollbar (theme-independent):**
- Light: thumb `rgba(0,0,0,0.22)`, hover `rgba(0,0,0,0.34)`, border 2px transparent
- Dark: thumb `rgba(255,255,255,0.24)`, hover `rgba(255,255,255,0.36)`, border 2px transparent
- Corner radius: 8px, width 8px

**Accent Colors (8 swatches for vault customization):**
```
HUES = [222, 158, 32, 268, 344, 188, 46, 12]
```
- Default accent: `#3d63dd` (hue 222)
- Light pastels (for swatches): `hsl({hue} 52% 84%)`
- Dark shade (swatch border): `hsl({hue} 36% 74%)`
- Active swatch shadow: `inset 0 0 0 2px var(--bg), 0 0 0 2px hsl({hue} 45% 48%)`

**Semantic Colors (in history track):**
- Create: `#3fa45a` (green)
- Edit: Accent color (default `#3d63dd`)
- Rename: `#d9a93d` (gold)
- Delete: `#d96a6a` (red)

**Special Overlays:**
- Accent soft (light accent): `{accent}22` (22% opacity)
- Selection background: Uses `accentSoft` (`accent + '22'`)

### 1.2 Typography

**Font Families:**
- Sans (system): `system-ui, -apple-system, 'Segoe UI', sans-serif`
- Serif (prose/editor): `'Newsreader', Georgia, serif`
- Mono: `'JetBrains Mono', ui-monospace, Menlo, monospace`

**Fonts Loaded (WOFF2, self-hosted):**
- JetBrains Mono: weights 400, 500, 600
- Newsreader: weights 400, 500
- Both have extended latin and latin unicode ranges

**Editor Typography:**
- Markdown prose: **Serif 15.5px, line-height 1.8**, padding `44px 40px 140px`
  - When centered (writing column): width 760px, max-width 100%
  - When not centered: width 100%
- Code files: **Mono 13px, line-height 1.7**, padding `30px 36px 140px`, tab-size 2

**UI Typography (various contexts):**
- Headings (h1, Connect screen): 25px, weight 600, letter-spacing -0.02em
- Modals: 16px, weight 600, letter-spacing -0.01em
- Sidebar switcher: 14px, weight 600, letter-spacing -0.01em
- File tree rows: 13.5px, weight 400 (inactive), 500 (folder or active)
- Tab label: 12.5px, weight 500 (inactive), 600 (active)
- History bar: 12px (status), 11.5px (log), 10.5px (mono fingerprint)
- Buttons: 13–14px, weight 500–600
- Labels/titles: 11px–12.5px, uppercase, weight 600, letter-spacing 0.05–0.07em

### 1.3 Spacing & Dimensions

**Window Defaults:**
- Min width: 200px (sidebar) + 300px (editor) = 500px practical
- Min height: 400px

**Sidebar:**
- Width default: 266px
- Min: 200px, max: 460px
- Resize handle: width 7px (hover area includes ±3px margin), cursor `col-resize`
- Header (vault switcher): height 47px, padding `0 14px`
- "Files" label row: height 30px, padding `9px 9px 7px`
- File tree row: height 29px, padding `2px 8px 12px` (container), per-row padding `0 8px` right, `7 + depth*15` left
- Row chevron/icon indent: 16px wide, centered
- Depth indent: 15px per level

**Tab Bar:**
- Height: 48px (row), padding `0 16px 0 0`, border-bottom 1px
- Tab item: max-width 220px, padding `0 8px 0 12px`, gap 7px, border-right 1px
- Tab active top accent: `inset 0 2px 0 {accent}`
- Close button: 17×17px, opacity 0.7
- Status row (save/count): padding `7px 18px`

**Editor Main Area:**
- Scrollable region: padding `44px 40px 140px` (content starts 44px down)
- Centered prose width: 760px

**History Bar:**
- Height minimum: 96px, maximum: 640px, collapse threshold: 72px
- Status row: height 38px, padding `0 9px 0 15px`
- Track row: margin `0 16px 11px`
- Playhead handle: width 24px, height 28px, margin-top -14px
- Playhead line: width 2px, margin-left -1px, border-radius 1px

**Modals & Dropdowns:**
- Modal width: `min(424px, 92vw)`, padding 20px, border-radius 16px
- Modal max-height: 92vh, overflow-y auto
- Dropdowns: width varies (168–200px), padding 4–6px, border-radius 11–12px
- Context menus: width 156–200px, padding 4–5px, border-radius 10px

**Spacing Scale:**
- Gaps: 1, 2, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 26, 28, 32, 34
- Padding: 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20
- Margin: typically 2, 3, 4, 6, 22, 26, 28, 32, 34

### 1.4 Border Radii & Borders

**Border Radius:**
- Tiny: 4px (inline rename input, close button hover)
- Small: 6–7px (icon buttons, menu items, tab row buttons)
- Medium: 8px (vault-switcher chevron, modal menus)
- Large: 9–10px (modal buttons, context menus)
- XL: 11–12px (modal menus, emoji picker)
- Pill: 14px (vault list card), 20px (time-travel pill)
- Modal: 16px

**Borders:**
- Dividers: 1px solid `--line`
- Input/active field: 1px solid `--accent`
- Button: 1px solid `--line` (secondary), 0 (primary)
- Scrollbar thumb: 2px transparent (background-clip padding-box)

### 1.5 Shadows & Elevation

**Shadows:**
- Subtle button: `0 1px 2px rgba(28,25,23,0.08)` (New button on Connect)
- Light menu: `0 12px 32px rgba(28,25,23,0.13)` (vault menu, small)
- Medium menu: `0 12px 32px rgba(28,25,23,0.14)` (New menu, Files menu)
- Strong modal: `0 24px 60px rgba(28,25,23,0.28)` (entry/customize modals)
- Context/history: `0 10px 28px rgba(28,25,23,0.15)` (location menu, history overlay)
- Playhead handle: `0 2px 6px rgba(28,25,23,0.22)`
- All shadows context-darkened slightly in dark theme

### 1.6 Animations & Transitions

**Keyframes:**

```css
@keyframes aspPulse { 
  0%, 100% { opacity: 1 } 
  50% { opacity: 0.32 } 
}
```
Duration: typically 2.4s, easing ease-in-out, infinite

```css
@keyframes aspSpin { 
  to { transform: rotate(360deg) } 
}
```
Duration: 0.7s, easing linear, infinite

**Transitions:**
- Chevron rotation: `transform .15s`
- Hover backgrounds: `.1s` or none (instant)
- Tab drag opacity: instant
- File tree font/color: instant
- Box-shadow (swatch): `.1s`
- Height (history bar): `.16s ease`
- Modal backdrop: `blur(2px)` filter

### 1.7 Frontmatter Styles

**Card (default frontmatterStyle: 'Card'):**
- Background: `--bg-input`, borders left/right 1px `--line`
- Margin: `-16px 0`, padding `3px 16px`
- Font: 13.5px, line-height 1.95
- Top: border-top 1px, border-radius 11px 11px 0 0, margin-top 4px, padding-top 10px
  - Prefix: "Properties", uppercase, 10px, weight 600, letter-spacing 0.07em
- Bottom: border-bottom 1px, border-radius 0 0 11px 11px, padding-bottom 10px, margin-bottom 34px
- Key: font 11px, uppercase, weight 500, letter-spacing 0.04em, min-width 104px, color `--faint`
- Value: color `--text`
- Array values: color `--accent`

**Banner (frontmatterStyle: 'Banner'):**
- Title: 23px, weight 600, letter-spacing -0.02em, line-height 1.25, margin `2px 0 6px`
- Meta: inline-block, margin `0 18px 4px 0`, font 12.5px, color `--text3`
  - Key: 10.5px, uppercase, letter-spacing 0.04em, margin-right 5px, color `--faint`
- End divider: height 0, border-bottom 1px `--line`, margin `16px -600px 26px`
- Array color: `--accent`

**Below (frontmatterStyle: 'Below'):**
- Title: 30px, weight 600, letter-spacing -0.025em, line-height 1.15, margin `2px 0 14px`
- Meta: flex row, font 13px, line-height 1.85, color `--text3`
  - Key: flex none, min-width 92px, font 11px, uppercase, letter-spacing 0.04em, color `--faint`
- End divider: height 0, border-bottom 1px `--line`, margin `18px -600px 28px`
- Array color: `--accent`

---

## 2. Global Layout

### 2.1 Document Structure

**HTML Root:**
```html
<!doctype html>
<html lang="en" data-theme="light">  <!-- toggles to "dark" -->
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>asp — Vault Editor</title>
  </head>
  <body>
    <div id="root"></div>
  </body>
</html>
```

**Global CSS:**
- `*`: box-sizing border-box
- `html, body`: margin 0, padding 0, height 100%
- `#root`: height 100%
- `body`: font-family system-ui sans-serif, -webkit-font-smoothing antialiased, background `--bg`, color `--text`
- `::selection`: background rgba(61, 99, 221, 0.16) — accent at 16% opacity
- Placeholder: color `--faint`

### 2.2 Window Layout

**Fixed-size window (Tauri or web):**
- 100% viewport, flex column, no scroll (children control scroll regions)
- Typical launch size: ~1200×800

### 2.3 Main Container Flex Structure

**Root flex (`position: fixed, inset: 0, display: flex, flexDirection: column`):**
- Top: content area (flex: 1, flex-direction row)
- Bottom: history/log bar (height 38–640px, flex-none)

**Content area (flex: 1, flex-direction row):**
- Left: sidebar (width 200–460px, flex-none, flex-direction column)
- Middle: resize handle (width 7px, flex-none, cursor col-resize)
- Right: editor pane (flex: 1, flex-direction column, minWidth 0)

**Editor pane (flex: 1, flex-direction column):**
- Top: tab bar row (height 48px, flex-none, border-bottom)
- Upper-top: status row (height ~30px, flex-none, border-bottom)
- Optional: time-travel banner (flex-none, height ~38px)
- Main: scrollable editor (flex: 1, overflow auto)

**Sidebar (flex-direction column):**
- Vault switcher header (flex-none, height 47px)
- Vault menu dropdown (absolute, z-index 40–41)
- Files label + buttons (flex-none, height ~30px)
- File tree (flex: 1, scrollable)

---

## 3. Connect Screen

**Layout:** Fixed fullscreen, flex column, center-aligned.

**Background:** `var(--bg-sub)`, padding 32px, overflow auto

**Card Container:**
```
width: min(452px, 94vw)
display: flex
flex-direction: column
```

### 3.1 Header Row

- Display: flex, align-center, gap 11, margin-bottom 34
- App logo (left):
  - 26×26px, border-radius 7px, background accent
  - Inside: white circle 9×9px, border-radius 50%
- App name: "asp", mono 16px, weight 600, letter-spacing -0.01em
- Platform indicator (right): flex, gap 6, font 12px, color `--faint`
  - Square 8×8px, border-radius 2px
  - Desktop: background accent, text "On this computer"
  - Web: background `--faint2`, text "Saved in this browser"
- Theme button (far right): 28×28px icon, border-radius 8, border `--line`, bg `--bg`

### 3.2 Headline

- "Your vaults", 25px, weight 600, letter-spacing -0.02em, margin `0 0 22px`

### 3.3 Action Buttons

Two flex: 1 buttons, gap 10:

**New Vault button:**
- height 46px, background `--text`, color `--bg`, border-radius 11
- PlusIcon + "New Vault" text, weight 500, font 14px
- Shadow: `0 1px 2px rgba(28,25,23,0.18)`

**Connect Vault button:**
- height 46px, border `--line`, background `--bg`, color `--text2`, border-radius 11
- ConnectIcon + "Connect Vault" text, weight 500, font 14px

### 3.4 Vault List Card (when saved.length > 0)

- Margin-top 26, margin-bottom 9
- Label: 11px, weight 600, uppercase, letter-spacing 0.07em, color `--faint2`, padding-left 3
- Card: border `--line`, border-radius 14, overflow hidden, background `--bg`
- Each row:
  - Display: flex, gap 13, padding `13px 15px`, cursor pointer
  - Border-top: 1px `--line` (not first)
  - Hover: background `--bg-sub` (asp-hover-list)
  - Avatar: 34×34px, border-radius 10, background `hsl({hue} 44% 94%)`, border `1px hsl({hue} 36% 86%)`
    - Emoji: font 18.36px (34 * 0.54), line-height 1
    - Monogram: font 13.6px (34 * 0.4), weight 600, color `hsl({hue} 42% 40%)`
  - Content (flex: 1, minWidth 0):
    - Name: 14.5px, weight 500, color `--text`, overflow ellipsis
    - Location: flex, gap 6, margin-top 3, font 11px, color `--faint`
      - FolderIcon 12px (desktop) or GlobeIcon 12px (web)
      - Path or "Using browser storage"
  - Time/opening status (flex-none): 11.5px, color `--faint`
    - "Opening…" (dimmed 0.55 opacity if loading)
    - Relative time string (e.g., "2 hours ago") via relTime()
  - Chevron: 15px, stroke `#cfc9c1` (only non-loading)
- Loading vault row (opacity 0.55, non-interactive): same but no chevron, text "Opening…"

### 3.5 Device Fingerprint (footer)

- Margin-top 28, font 11.5px, color `--faint2`
- UserIcon 12×12px + text "This device · {fingerprint}"

---

## 4. Editor Screen

### 4.1 Sidebar: Vault Switcher

**Header row (47px):**
- Padding `0 14px`, gap 11, cursor pointer, hover background `--line`
- Avatar: 28×28px, border-radius 8
- Content (flex: 1, minWidth 0):
  - Display name: 14px, weight 600, letter-spacing -0.01em
  - Status row: flex, gap 6, margin-top 2
    - Pulse dot: 6×6px, border-radius 50%, background accent, animation aspPulse 2.4s infinite
    - Sync summary: 11px, color `--faint`, text "Synced" or "Synced · {N} peer(s)"
- Caret icon: down-pointing, rotated 180° when menu open, transition 0.15s

**Dropdown Menu (absolute, z-index 40-41):**
- Positioned: top `calc(100% - 4px)`, left 8, right 8
- Background `--bg`, border `--line`, border-radius 12
- Shadow: `0 12px 32px rgba(28,25,23,0.13)`
- Padding 6, gap 2, max-height `calc(100vh - 80px)`
- Sections:
  1. **"Switch vault" header:** 10.5px, weight 600, uppercase, letter-spacing 0.06em, color `--faint2`, padding `7px 9px 4px`
  2. **Vault list (scrollable):** max-height `calc(100vh - 320px)`, hover `--bg-sub`
     - Each vault: gap 11, padding `8px 9px`, border-radius 8, cursor pointer
     - Avatar 26×26px, border-radius 8
     - Name: 13.5px, weight 500, color `--text`
     - Path: 10.5px, mono, color `--faint2`
     - CheckIcon (if active): stroke accent
  3. **Divider:** 1px `--line`, margin `4px 6px`
  4. **"Customize this vault…":** gap 10, padding `8px 9px`, WandIcon
  5. **"Share this vault…":** gap 10, padding `8px 9px`, ShareIcon
  6. **"Remove this vault…":** gap 10, padding `8px 9px`, TrashIcon, color `--text2`
  7. **Divider:** 1px `--line`, margin `4px 6px`
  8. **"Open another folder…" (desktop) / "New vault…" (web):** gap 10, padding `8px 9px`, FolderIcon or PlusIcon

### 4.2 Sidebar: Files Section

**Header row (~30px):**
- Display: flex, align-center, gap 1, padding `9px 9px 7px`, position relative
- Label: "Files", 11px, weight 600, uppercase, letter-spacing 0.06em, color `--faint2`, flex 1, padding-left 3
- New button: 24×24px, border-radius 6, background `transparent` (or `--line` if open), color varies
  - PlusIcon 16px
  - Dropdown menu (top 34, right 38, z-index 44-45):
    - Width 168px, border-radius 11, shadow `0 12px 32px rgba(28,25,23,0.14)`, padding 5
    - "New file" + NewFileIcon
    - "New folder" + NewFolderIcon
- Expand/collapse all button: 24×24px, ExpandCollapseIcon, state-based
- More menu button: 24×24px, DotsIcon
  - Dropdown menu (top 34, right 8, z-index 44-45):
    - Width 186px, border-radius 11, shadow, padding 5
    - "Show hidden files" + EyeIcon (on/off)
    - "Pretty filenames" + CheckIcon or spacer

### 4.3 Sidebar: File Tree

**Container:**
- Flex: 1, overflow-y auto, padding `2px 8px 12px`
- Class: asp-scroll (custom scrollbar styling)
- Box-shadow: `inset 0 0 0 2px {accent}` when dragging files to root

**Row Rendering (virtualized):**
- ROW_H = 29px fixed
- OVERSCAN = 8 rows (render 8 rows above/below viewport)
- Total height container: `total * 29px`

**Each Row (absolute positioned):**
- Height 29px, display flex, gap 6, align-items center
- Padding: right 8px, left `7 + depth*15` (depth indent)
- Border-radius 7, cursor pointer, user-select none
- Font 13.5px, weight 400 (file) or 500 (folder or active)
- Font-style italic (if hidden or pretty italic)
- Color:
  - Hidden files: `--faint2`
  - Active file: `--text`
  - Inactive file: `--text2`
  - Folders: `--text2`
- Background:
  - Selected or multi-select: `{accentSoft}` (accent + '22')
  - Drop target: `{accentSoft}` + `inset 0 0 0 2px {accent}`
  - Context target: `inset 0 0 0 1.5px {accent}`
- Box-shadow transitions on hover/select

**Chevron (folder icon):**
- 16px wide column, centered
- ChevronRight 11px, color `--faint`, transition `transform .14s`
- Rotation: 0° (collapsed), 90° (expanded)

**File Icon:**
- 16px column, flex-centered
- FileIcon 13px
- Color: `{accent}` (if active), `--faint2` (if inactive), `--faint2` (if hidden)

**Label:**
- Flex: 1, overflow hidden, text-overflow ellipsis, white-space nowrap

**Inline Rename (when renaming):**
- Input: width flex:1, font inherit, border `1px {accent}`, border-radius 4, padding `1px 5px`, background `--bg`, color `--text`, font 13.5px
- No outline
- AutoFocus, spellCheck false
- onBlur commits

**Drag & Drop:**
- Draggable when not renaming
- effectAllowed: move
- dataTransfer: text/plain (path) + application/x-asp-path (path)
- Drop target (folders): cursor pointer, propagation stops
- Root drop: highlight with accent box-shadow

### 4.4 Sidebar Resize Handle

- Width 7px, cursor `col-resize`, margin `0 -3px`, flex-none
- Line: 1px `--line`, alignSelf stretch
- Hover: line background changes to `{accent}`

---

## 5. Editor Screen: Main Content Area

### 5.1 Tab Bar

**Container (height 48px, flex: 1, minWidth 0):**
- Class: tab-strip, scrollbar-width none (hides scrollbar)
- Display flex, align-items stretch, overflow-x auto, overflow-y hidden

**Each Tab (flex-none, max-width 220px):**
- Role: tab, aria-selected={isActive}
- Display flex, align-items center, gap 7, padding `0 8px 0 12px`
- Border-right `1px --line`
- Cursor pointer, user-select none, touch-action none
- Font 12.5px, weight 500 (inactive) or 600 (active)
- Color: `--text` (active), `--text3` (inactive)
- Background: `--bg` (active), transparent (inactive)
- Box-shadow: `inset 0 2px 0 {accent}` (active)
- Hover: varies per state
- @dnd-kit drag: transform & transition (neighbors slide), opacity 0.55, z-index 1
- Title: full path

**Tab Label (or inline rename input):**
- Overflow hidden, text-overflow ellipsis
- Rename input: width 130, font inherit, border `1px {accent}`, background `--bg`, color `--text`, border-radius 4, padding `1px 5px`

**Close Button (×):**
- 17×17px, flex-none, border-radius 4, color inherit, opacity 0.7
- Background transparent, border none, padding 0
- Font 14px, line-height 1
- Cursor pointer
- Stops propagation (e.stopPropagation)
- Middle-click closes (onMouseDown button 1)

**Interactions:**
- Click: select tab (collapse multi-select)
- Double-click: start inline rename
- Drag (4px activation distance): reorder within strip
- Right-click: context menu
- Middle-click: close
- Space+Arrow (when focused): reorder, accessible

**External Drag-and-Drop:**
- Native HTML5 (not @dnd-kit)
- Receives dataTransfer MIME "application/x-asp-path"
- File dragged from tree opens as tab (no move)

### 5.2 Status & Header Rows

**Tab bar row (height 48px, flex-none):**
- Display flex, align-center, gap 10, padding `0 16px 0 0`, border-bottom `--line`
- Left: TabBar (flex: 1, minWidth 0)
- Right: theme button (28×28px)

**Content Status Row (height ~30px, flex-none):**
- Display flex, align-items center, justify-content flex-end, gap 8
- Padding `7px 18px`, border-bottom `--line`
- Flex-none items (right-aligned):
  - Save indicator dot: 6×6px, border-radius 50%, background `#3fa45a` (saved) or `#d9a93d` (saving), transition 0.2s
  - "Saved" or "Saving…": 11.5px, color `--faint`
  - Divider: 1px `--line`, height 11, margin `0 2px`
  - Word count: 11.5px, mono, tabular-nums, color `--faint2`

**Time-Travel Banner (if playhead != null && playhead < now):**
- Height ~38px, flex-none, display flex, align-items center, gap 12
- Padding `9px 18px`, background `{accentSoft}`, border-bottom `1px {accent}33`
- ClockIcon (stroke accent)
- Message: flex 1, minWidth 0, font 12.5px, color `--text2`
  - "Viewing this vault as it was on **{date}** · read-only"
  - Date: formatLocaleString()
- Buttons (flex-none):
  - "Restore this version": font 12px, weight 500, bg accent, color `--bg`, border-radius 7, padding `6px 12px`
  - "Return to now": font 12px, weight 500, bg `--bg`, border `--line`, color `--text2`

### 5.3 Editor Container (the Live WYSIWYG Area)

**Outer scroll region:**
- Flex: 1, minHeight 0, overflow-y auto, overflow-x hidden
- Display flex, justify-content center, align-items flex-start
- Class: asp-scroll

**Editor div (contentEditable):**

**Markdown prose styling:**
- Width: 760px (if centered), 100% (if not)
- Max-width 100%, min-height 100%
- Font: Newsreader 15.5px, line-height 1.8, color `--text`
- Padding: `44px 40px 140px` (top, left, bottom; 140px bottom gives room to scroll past last line)
- Background: transparent, outline: none, white-space pre-wrap, word-break break-word
- CSS variable: `--accent` set to editor accent (used in markdown rendering)

**Code file styling:**
- Width 100%, max-width 100%, min-height 100%
- Font: Mono 13px, line-height 1.7, color `--text`
- Padding: `30px 36px 140px`
- Tab-size: 2
- Background: transparent, outline none, white-space pre-wrap

**Markdown Rendering Classes:**
- `.cm-mark` (syntax markers): font-size 0, color transparent (invisible but in DOM for byte-exact round-trip)
- `.cm-code` (inline code): mono 0.86em, background `--bg-input`, padding `1px 5px`, border-radius 5px
- `.cm-link` (links): color `--accent`, border-bottom `1px --accent`, cursor pointer (if data-href)
- `.cm-img` (inline images): max-height 1.4em, vertical-align middle, cursor pointer (if data-href)
- `.cm-ul` (unordered lists): padding-left 1.5em
  - `::before`: bullet "•", left 0.4em, top -0.02em, weight 700, color `--accent`
- `.cm-task` (checkboxes): padding-left 1.85em
  - `::before`: 16×16px box, border 1.6px `--faint2`, border-radius 4, left 0.1em, top 0.28em, cursor pointer
  - `.cm-task-done::before`: background & border `--accent`
  - `.cm-task-done::after`: checkmark (rotated 43°, white border), left calc(0.1em + 5.5px), top calc(0.28em + 2.5px)
  - `.cm-task-done .cm-body`: color `--faint`, text-decoration line-through
- `.cm-task-box` (hit zone): position absolute, width 1.75em, height 1.55em, cursor pointer, user-select none
- `.cm-quote` (blockquotes): margin 0, border-left `3px color-mix(in srgb, {accent} 33%, transparent)`
  - Padding `1px 0 1px 14px`, color `--text3`, font-style italic
  - Margin-top 8px (not after another quote), margin-bottom 8px (not before another quote)
- `.tbl-scroll` (table container): overflow-x auto, max-width 100%, margin-top 16px, display flex
- `.tbl-grid` (table): display table, min-width 100%, border-collapse collapse
- `.tcell` (table cell): padding `6px 13px`, border `1px --line`, font 14px, line-height 1.5, white-space nowrap, vertical-align top
- `.tbl-head .tcell`: weight 600, background `--bg-input`
- `.md-diagram` (mermaid): margin `4px 0 18px`, padding 14px, background `--bg-input`, border `1px --line`, border-radius `0 0 9px 9px`, text-align center, overflow-x auto

**Editor Interactions:**
- contentEditable when not readOnly, false when readOnly (opacity 0.92)
- onInput: debounced rehighlight (320ms), onChange
- onMouseDown: link/image click (openExternal), task checkbox toggle
- onPaste: plain text only (insertText)
- onCompositionStart/End: full rehighlight on end
- Diagram rendering: async mermaid via loadMermaid, debounce 200ms

---

## 6. Editor Screen: History Bar (Bottom)

### 6.1 Status Row (38px)

**Layout:** Display flex, align-center, height 38, padding `0 9px 0 15px`, gap 10, flex-none

**Left side:**
- Location icon: FolderIcon 12px (desktop) or GlobeIcon 12px (web), stroke `--faint2`, click reveals OS file manager (desktop)
- Location text: mono (desktop), inherit (web), 12px, color `pathCopied ? {accent} : --text2`, cursor pointer (desktop), max-width 190, overflow ellipsis, click copies path
  - "Copied path" feedback (1.2s)
  - Context menu: "Copy path", "Open in file manager"
- Fingerprint: mono 10.5px, color `--faint2`, white-space nowrap
- Time-travel pill (if timeTravel): mono 11px, padding `2px 9px`, border-radius 20, background `{accentSoft}`, color `{accent}`, weight 500, text `fmtFull(playhead)`

**Right side:**
- Tab buttons (History/Log):
  - Container: display flex, background `--line`, border-radius 8, padding 2
  - Each: gap 6, height 24, padding `0 11px`, border-radius 6, cursor pointer, font 12px, weight 500, font inherit
  - Base: background transparent, color `--text3`, border none
  - Active: background `--bg`, color `--text`, box-shadow `0 1px 2px rgba(0,0,0,0.08)`

### 6.2 History Tab (when histOpen)

**Container:** Flex column, flex: 1, minHeight 0, border-top `--line`

**Header row:** Padding `6px 14px 2px`, display flex, align-items center, gap 8
- Row count: 11px, color `--faint2`, mono, tabular-nums, flex 1
  - "{N} rows" or "{visible} / {total} rows" if time-traveling
- Zoom buttons (flex-none): border `1px --line`, border-radius 7, overflow hidden
  - Minus (zoom out, 1.8x): 26×24px, bg `--bg`, color `--text3`, border-right `1px --line`
  - Plus (zoom in, 0.55x): 26×24px, bg `--bg`, color `--text3`
- "Now" button: 12px, weight 500, color `{timeTravel ? --text2 : --faint2}`, bg `--bg`, border `--line`, border-radius 7, padding `4px 12px`

**Track area (scrollable):**
- Ref: trackRef, onPointerDown (scrub/jump), onWheel (zoom)
- Margin `0 16px 11px`, position relative, flex 1, cursor crosshair, touchAction none
- Background line: absolute, top 50%, height 1, background `--line`
- Axis ticks: positioned vertically at % offsets
  - Vertical line: 1px `--line`
  - Label: 9.5px, mono, color `--faint2`, positioned below
- Event circles (sampled):
  - Position: absolute, left `{pct}%`, top 50%, size 18×18px, margin -9 -9
  - Center dot: 9×9px, border-radius 50%
  - Filled (past): background `colorForKind(kind, accent)`
  - Unfilled (future): background `--bg`, border `1.5px colorForKind`
  - Opacity: 1 (past), 0.5 (future)
  - Cursor: pointer, onclick jump to timestamp
- Now line: dashed, left `{nowPct}%`, border-left `1px dashed --faint2`
- Playhead:
  - Line: position absolute, left `{playPct}%`, top/bottom 3, width 2, margin-left -1, background `{accent}`, border-radius 1, z-index 5
  - Handle: position absolute over line, top 50%, width 24, height 28, margin-top -14, border-radius 8
    - Background `{accent}`, border `2px --bg`, box-shadow `0 2px 6px rgba(28,25,23,0.22)`, cursor ew-resize
    - onPointerDown: drag to scrub playhead

### 6.3 Log Tab (when logOpen)

**Container:** Flex column, flex: 1, minHeight 0, border-top `--line`

**Header row:** Padding `6px 12px 2px 16px`, display flex, align-items center, gap 8
- Event count: 11px, color `--faint2`, tabular-nums, flex 1
- Copy button: 26×24px, transparent, border none, cursor pointer, color `--text3`
  - CheckIcon (green) if just copied, CopyIcon otherwise
  - Onclick: copy all log lines, feedback 1.4s

**Log lines (scrollable, class asp-scroll):**
- Flex 1, minHeight 0, overflow-y auto, padding `2px 14px 10px`
- Each line: display flex, gap 11, padding `1.5px 6px`, hover background `--bg-sub`, border-radius 4, mono 11.5px, line-height 1.7, white-space nowrap
  - Time: color `--faint2`, flex-none
  - Level: color `logColor(level, accent)`, flex-none, width 40, weight 500 (INFO, WARN, ERROR)
  - Message: color `--text2`, flex 1, overflow hidden, text-overflow ellipsis
  - Right-click: context menu "Copy line", "Copy all"

### 6.4 History Bar Resize Handle

- Height 7px, flex-none, cursor `row-resize`, margin `-3px 0`, z-index 6, display flex, align-items center
- Line: height 1, width 100%, background `--line`
- Hover: line changes to `{accent}`

---

## 7. Modals & Menus

### 7.1 Customize Modal

**Backdrop:** Position fixed, inset 0, z-index 74, background `var(--overlay)`, backdrop-filter `blur(2px)`

**Modal (z-index 75):**
- Position fixed, top 50%, left 50%, transform translate(-50%, -50%)
- Width `min(424px, 92vw)`, max-height 92vh, overflow-y auto
- Background `--bg`, border-radius 16, padding 20, gap 16, flex column
- Box-shadow: `0 24px 60px rgba(28,25,23,0.28)`
- OnKeyDown: Escape closes, Enter saves

**Header (flex row, gap 13):**
- Avatar: 46×46px, border-radius 13
- Title + subtitle
  - "Customize vault", 16px, weight 600, letter-spacing -0.01em
  - "Set a name, color, and icon.", 12.5px, color `--text3`

**Name field:**
- Label: "Name", 11px, uppercase, weight 600, letter-spacing 0.05em, color `--faint2`
- Input: 100%, font inherit, 14px, color `--text`, bg `--bg-input`, border `1px --line`, border-radius 10, padding `10px 12px`, outline none

**Color swatches:**
- Label: "Color", 11px, uppercase, weight 600, letter-spacing 0.05em
- Grid: display flex, gap 10, flex-wrap wrap
- Each swatch: 28×28px, border-radius 8, cursor pointer, background `hsl({hue} 52% 84%)`
  - Border (inactive): `inset 0 0 0 1px hsl({hue} 36% 74%)`
  - Box-shadow (active): `inset 0 0 0 2px var(--bg), 0 0 0 2px hsl({hue} 45% 48%)`
  - Transition: box-shadow 0.1s

**Icon section:**
- Label: "Icon", 11px, uppercase, weight 600, letter-spacing 0.05em
- Remove button (if emoji set): small button, gap 5, font 12px, weight 500, border `--line`, border-radius 7, XIcon + "Remove icon"
- Emoji picker (border `--line`, border-radius 12, height 214, display flex flex-column):
  - Search input: 100%, font 12.5px, bg `--bg-input`, border `--line`, border-radius 7, padding `4px 10px`, placeholder "Search emojis"
  - Category tabs (if not searching): flex row, padding `5px 6px`, gap 1, border-bottom `--line`, flex-none
    - Each: flex 1, height 28, border-radius 7, cursor pointer, font 16px, line-height 1
    - Background: `--line` (active), transparent (inactive)
  - Results grid (scrollable, asp-scroll): padding `6px 7px 8px`, grid cols 8, gap 1
    - Each emoji: 34×34px, flex, justify-center, align-center, font 21px, hover `--bg-sub`, cursor pointer
    - Empty state: font 13px, color `--faint`, text "No emoji found"

**Buttons (flex, justify-end, gap 8, margin-top 2):**
- Cancel: 13px, weight 500, color `--text2`, bg `--bg`, border `--line`, border-radius 9, padding `8px 16px`
- Save: 13px, weight 500, color `--bg`, bg `--text`, border none, border-radius 9, padding `8px 18px`

### 7.2 Share Modal (derived from entry modal pattern)

Width `min(424px, 92vw)`, similar styling to entry modal.

**Fields:**
- Connection code (read-only, mono, displayable)
- "Require access key?" toggle
- Access key (if required)
- "Copy code" button (with feedback: "Copied!" for 1.2s)
- Share URL or instructions

### 7.3 Remove Vault Modal

Confirmation dialog:
- "Remove {vault name}?"
- "This cannot be undone."
- Buttons: "Keep it" (secondary), "Move to Trash" or "Remove" (danger red color)

### 7.4 Entry Modal (New/Connect Vault)

**Backdrop & positioning:** Same as Customize

**Modal (z-index 58-59):**
- Width `min(424px, 92vw)`, gap 15, flex column
- Background `--bg`, border-radius 16, padding 20
- OnKeyDown: Escape cancels (if not connecting), Enter submits (or Cmd/Ctrl+Enter for textarea)

**Header:**
- Title: "New vault" or "Connect a vault", 16px, weight 600, letter-spacing -0.01em
- Subtitle: 12.5px, color `--text3`
  - New (desktop): "Name it and choose a folder — everything syncs automatically."
  - New (web): "Name it and start writing — it saves in this browser and syncs automatically."
  - Connect: "Paste a code someone shared with you."

**Fields:**
- **Name** (New only):
  - Label: "Name", uppercase, 11px, weight 600, letter-spacing 0.05em
  - Input: width 100%, inherit, 14px, placeholder "My vault"
- **Invite code** (Connect only):
  - Label: "Invite code", uppercase, 11px, weight 600
  - Textarea: 2 rows, mono 12.5px, line-height 1.5, placeholder "Paste the code someone shared with you"
- **Access key** (Connect only):
  - Label: "Access key — if required", 11px, uppercase, weight 600, lighter subtext
  - Input: type password, mono 12.5px, placeholder "Leave blank if you weren't given one"
- **Location** (Desktop only, both New and Connect):
  - Label: "Location" or "Save to", 11px, uppercase, weight 600
  - Folder picker button: gap 9, align-center, bg `--bg-input`, border `--line`, border-radius 10, padding `10px 13px`, cursor pointer
    - FolderIcon 15px, stroke `--faint`
    - Path display (mono 12px, color `connectDest ? --text : --faint`), flex 1, ellipsis
    - "Choose…" text, 12px, color `--faint`

**Buttons (flex, justify-end, gap 8, margin-top 2):**
- Cancel: 13px, weight 500, color `--text2`, bg `--bg`, border `--line`, border-radius 9, padding `8px 16px`
- Submit: minWidth 108, height 38, padding `0 18px`, border-radius 9, weight 500, font 13px, font inherit
  - Disabled: background `--faint2`, cursor default
  - Enabled: background `--text`, color `--bg`, cursor pointer
  - Spinner (if connecting): 13×13px border `2px white66`, border-top `--bg`, border-radius 50%, animation aspSpin 0.7s linear infinite
  - Text: "New" → "Create vault" or "Connect" → "Connect" (or "Connecting…")

### 7.5 Context Menus (File Tree, Tab Bar, Connect Screen)

**Backdrop:** Fixed, inset 0, z-index 60 (or 62 for vault list), click closes

**Menu (z-index 61 or 63):**
- Position fixed, x/y clamped to stay on-screen
- Width 156–188px, background `--bg`, border `--line`, border-radius 10
- Shadow: `0 12px 32px rgba(28,25,23,0.16)` (file/tab) or `0 10px 28px rgba(28,25,23,0.15)` (connect)
- Padding 4–5px, gap 0

**Menu items:**
- Display flex, align-center, padding `7px 11px`, border-radius 7, cursor pointer, font 13px
- Hover: background `--bg-sub` (normal) or `rgba(192, 57, 43, 0.10)` (danger/asp-hover-danger)
- Divider: height 1, background `--line`, margin `4px 6px`

**File tree context menu:**
- (Root) "New file", "New folder"
- (File/Folder) "Rename", "Delete"

**Tab bar context menu:**
- "Close"
- "Close Others"
- "Close to the Left"
- "Close to the Right"
- "Close All"
- Divider
- "Rename"
- "Delete"

**Connect screen vault context menu:**
- "Customize…"
- "Remove vault…" (danger color `#c0392b`)

**History/log context menus:**
- Location: "Copy path", "Open in file manager"
- Log line: "Copy line", "Copy all"

---

## 8. States & Interactions

### 8.1 Hover States

**Classes:**
- `.asp-hover-row`: background `--line` on hover (file tree rows, sidebar items)
- `.asp-hover-soft`: background `--bg-sub` on hover (vault switcher menu items, log lines)
- `.asp-hover-list`: background `--bg-sub` on hover (vault list cards on Connect screen)
- `.asp-hover-danger`: background `rgba(192, 57, 43, 0.10)` on hover (delete/danger menu items)
- `.asp-icon-btn`: background `--line`, color `--text` on hover (icon buttons)

### 8.2 Active / Selected States

**File tree:**
- Active file: weight 500, color `--text`, icon color accent
- Multi-select: all selected files background `{accentSoft}`
- Active file also has `{accentSoft}` background
- Context target: `inset 0 0 0 1.5px {accent}`
- Drop target: `inset 0 0 0 2px {accent}` + `{accentSoft}` background

**Tab bar:**
- Active tab: weight 600, color `--text`, background `--bg`, top accent `inset 0 2px 0 {accent}`
- Inactive: weight 500, color `--text3`, background transparent

**Swatches (color picker):**
- Active: `inset 0 0 0 2px var(--bg), 0 0 0 2px hsl({hue} 45% 48%)`
- Inactive: `inset 0 0 0 1px hsl({hue} 36% 74%)`

### 8.3 Focus & Read-only States

**Editor:**
- Read-only: opacity 0.92, contentEditable false
- Focus: outline none, cursor inherits content
- Composition (IME): deferred highlight until composition ends

**Inputs/textareas:**
- Focus: border accent color, outline none
- Placeholder: color `--faint`

**Buttons:**
- Disabled: background `--faint2`, cursor default, no click handling
- Cursor changes: col-resize (sidebar), row-resize (history bar), pointer (clickables), crosshair (track)

### 8.4 Loading & Sync States

**Vault loading (Connect screen):**
- Row opacity 0.55, non-interactive, no menu, chevron hidden
- Text: "Opening…"

**Saving status (Editor header):**
- Dot color: `#3fa45a` (saved), `#d9a93d` (saving)
- Transition: 0.2s
- Text: "Saved" or "Saving…"

**Pulse indicator (vault switcher, status row):**
- Dot: 6×6px, `animation: aspPulse 2.4s ease-in-out infinite`
- Pulses at 50% opacity at midpoint

**Sync summary:**
- "Synced" (no peers)
- "Synced · N peer(s)" (with live peer count)

**History load indicator (entry modal):**
- Connecting: spinner + "Connecting…" or "Saving…"
- Spinner: 13×13px, border `2px white66`, border-top `--bg`, border-radius 50%, `animation: aspSpin 0.7s linear infinite`

### 8.5 Time-Travel States

**When playhead != null && playhead < now:**
- Editor background: opacity 0.92 (read-only appearance)
- Time-travel banner visible (yellow/gold banner with restore button)
- Playhead indicator visible on history track
- All edits disabled, read-only overlay

### 8.6 Empty & Null States

**No selection (editor main):**
- Large icon (FileIcon 40px)
- Message: "Select a note to start editing", 14px, color `--faint`

**No vaults (Connect screen):**
- "Your vaults" heading visible, no vault list card
- Action buttons (New/Connect) present

**Empty emoji search:**
- "No emoji found", 13px, color `--faint`

**No files in vault (tree):**
- Empty container with no rows, can still use New File / Folder

---

## 9. A Componentized Inventory

Every distinct UI component that GPUI must build:

### Core Components

1. **ThemeToggle** — Sun/moon icon button, toggles light/dark theme
2. **Avatar** — Hue-tinted badge with emoji or monogram letter
3. **FileIcon** — SVG file glyph (13px)
4. **ChevronRight** — SVG arrow, rotates on folder expand
5. **CaretDown** — SVG down arrow for dropdown toggle
6. **PlusIcon** — SVG add/plus icon
7. **FolderIcon** — SVG folder icon
8. **TrashIcon** — SVG delete/trash icon
9. **ShareIcon** — SVG share/network icon
10. **CheckIcon** — SVG checkmark
11. **ClockIcon** — SVG clock for history
12. **MinusIcon** — SVG minus for zoom
13. **UserIcon** — SVG person/user icon
14. **LinkIcon** — SVG link chain icon
15. **DesktopIcon** — SVG monitor/desktop icon
16. **WandIcon** — SVG wand/customize icon
17. **XIcon** — SVG close/cancel icon
18. **ConnectIcon** — SVG link/connect icon
19. **GlobeIcon** — SVG globe for web vaults
20. **ThemeIcon** — SVG sun/moon (conditional)
21. **EyeIcon** — SVG eye, on/off variant
22. **ListIcon** — SVG bulleted list
23. **CopyIcon** — SVG copy/duplicate
24. **DotsIcon** — SVG three-dots menu
25. **NewFileIcon** — SVG file with plus overlay
26. **NewFolderIcon** — SVG folder with plus overlay
27. **ExpandCollapseIcon** — SVG expand/collapse arrows

### Layout Components

28. **MainWindow** — Fixed fullscreen container with theme attribute
29. **ConnectScreen** — Center-aligned card with vault list
30. **EditorScreen** — Fixed flex layout: sidebar | resize | main | history-bar
31. **Sidebar** — Fixed width, flex column: header | Files label | FileTree
32. **VaultSwitcher** — Header row with avatar, name, caret, hover-to-open menu
33. **VaultMenu** — Dropdown with vault list, customize, share, remove options
34. **FileTree** — Virtualized scroll region with rows, depth indent, icons
35. **FileTreeRow** — Absolute-positioned row with chevron, icon, label, hover/select states
36. **SidebarResizeHandle** — Draggable resize zone, hover highlight
37. **MainContent** — Flex column: TabBar | StatusRow | [TimeTravelBanner] | Editor | HistoryBar
38. **TabBar** — Horizontal scrollable strip with dnd-kit sortable tabs
39. **Tab** — Individual tab with label, close button, drag handle, rename input
40. **StatusRow** — Save indicator, word count, inline row
41. **TimeTravelBanner** — Yellow banner with restore button when scrubbing history
42. **Editor** — ContentEditable with markdown/code rendering
43. **HistoryBar** — Status row + History/Log tabs with collapsible panels
44. **HistoryTrack** — Interactive timeline with ticks, events, playhead
45. **LogPanel** — Scrollable log lines with copy buttons
46. **HistoryBarResizeHandle** — Draggable handle to grow/shrink bar

### Modal Components

47. **ModalBackdrop** — Fixed overlay with optional backdrop blur
48. **CustomizeModal** — Name input, color swatches, emoji picker
49. **EmojiPicker** — Category tabs, search, grid of results
50. **EmojiGrid** — Grid layout of clickable emoji
51. **EntryModal** — New Vault / Connect Vault dialogs
52. **FolderPicker** — Desktop folder selection button + path display
53. **ShareModal** — Connection code display, access key, copy buttons
54. **RemoveVaultModal** — Confirmation with trash/permanent delete option
55. **ContextMenu** — Positioned floating menu with items, hover states
56. **FileContextMenu** — New file, new folder, rename, delete
57. **TabContextMenu** — Close, close others, close variants, rename, delete
58. **VaultContextMenu** — Customize, remove vault (connect screen)
59. **HistoryContextMenu** — Copy line, copy all (log)
60. **LocationContextMenu** — Copy path, open in file manager (history status)

### Form Components

61. **TextField** — Text input with label, border, hover/focus states
62. **TextArea** — Multi-line textarea with label, mono font option
63. **SecretField** — Password input, masked
64. **Button** — Primary, secondary, danger variants; disabled state
65. **IconButton** — Icon-only button, small, hover highlight
66. **Checkbox** — Task list checkbox (rendered in markdown)
67. **ColorSwatch** — 28×28px swatch with hover, active, inactive states
68. **CategoryTab** — Emoji category selector in picker

### Markdown Rendering

69. **CodeSpan** — Inline code with mono font, input background
70. **Link** — Styled link with underline, clickable
71. **Image** — Inline image with max-height, baseline align
72. **UnorderedList** — Indented list with bullet glyph
73. **ListItem** — Individual list item
74. **TaskItem** — Checkbox + label, toggle state
75. **TaskCheckbox** — Interactive checkbox in task line
76. **BlockQuote** — Left accent bar, italic, condensed spacing
77. **Table** — CSS table layout with horizontal scroll
78. **TableRow** — Table row with cells
79. **TableCell** — Table cell with border, padding
80. **TableHead** — Table header styling
81. **CodeBlock** — Pre-formatted code fence
82. **DiagramBlock** — Mermaid SVG preview or code fallback
83. **FrontmatterCard** — Frontmatter Key-Value rendering (Card style)
84. **FrontmatterBanner** — Frontmatter in header style (Banner)
85. **FrontmatterBelow** — Frontmatter with title above (Below)

### Utility Components

86. **Divider** — 1px line (horizontal or vertical)
87. **Spacer** — Flex spacer / gap
88. **Scrollbar** — Custom thin scrollbar styling (CSS only)
89. **Tooltip** — Hover text (title attribute)
90. **Badge** — Small inline label or indicator
91. **Spinner** — Loading spinner animation (aspSpin)
92. **PulseDot** — Small indicator dot (aspPulse animation)
93. **Overlay** — Semi-transparent modal backdrop
94. **Fade** — CSS transition for opacity
95. **Slide** — CSS transition for transform (dnd-kit)

---

## 10. Exact Pixel Dimensions (Summary)

| Element | Width | Height | Details |
|---------|-------|--------|---------|
| Window (default) | 1200px | 800px | Typical Tauri launch |
| Sidebar | 266px | 100% | Min 200, max 460 |
| Sidebar header | 266px | 47px | Vault switcher |
| Resize handle | 7px | 100% | Cursor col-resize |
| Tab bar row | 100% | 48px | Includes status buttons |
| Single tab | 220px max | 100% of row | Flex-none |
| Close button | 17px | 17px | On each tab |
| Status row | 100% | ~30px | Save indicator + count |
| Time-travel banner | 100% | ~38px | Icon + message + buttons |
| File tree row | 100% | 29px | Virtualized, fixed height |
| FileIcon | 13px | 13px | Inline in tree |
| ChevronRight | 11px | 11px | Folder indicator |
| Avatar (list) | 34px | 34px | Vault list card |
| Avatar (switcher) | 28px | 28px | Vault menu items |
| Avatar (customize) | 46px | 46px | Modal header |
| History bar min | 100% | 96px | HISTBAR_MIN |
| History bar max | 100% | 640px | HISTBAR_MAX |
| History bar default | 100% | 150px | prefs.histBarH |
| Track area | 100% - 32px | ~140px | Margin 16px each side |
| Playhead handle | 24px | 28px | Margin -14 top |
| Event circle | 18px | 18px | Margin -9 center |
| Emoji swatch | 28px | 28px | 8-column grid + 1px gap |
| Modal | min(424px, 92vw) | auto | Max 92vh |
| Divider | 100% | 1px | Horizontal |
| Divider (vertical) | 1px | 11px | In status row |

---

## 11. Critical CSS Classes

- `.asp-scroll` — Custom scrollbar (thin, rounded, theme-aware)
- `.tab-strip` — Tab bar container (hides scrollbar)
- `.asp-hover-row` — Hover background `--line`
- `.asp-hover-soft` — Hover background `--bg-sub`
- `.asp-hover-list` — Hover background `--bg-sub` (list items)
- `.asp-hover-danger` — Hover background rgba(192, 57, 43, 0.10)
- `.asp-icon-btn` — Icon button hover (background `--line`, color `--text`)
- `.cm-mark` — Hidden syntax marker (font-size 0, color transparent)
- `.cm-code` — Inline code (mono, padded, bg input)
- `.cm-link` — Styled link (accent color, underline)
- `.cm-ul` — Unordered list (bullet before)
- `.cm-task` — Task item (checkbox before)
- `.cm-task-done` — Completed task (filled checkbox, strikethrough text)
- `.cm-quote` — Blockquote (left accent bar, italic)
- `.tbl-scroll` — Table scroll region
- `.tbl-grid` — Table display:table wrapper
- `.tcell` — Table cell (border, padding)
- `.md-diagram` — Rendered mermaid diagram
- `.sb-resize` — Sidebar resize handle (hover line color)
- `.hb-resize` — History bar resize handle (hover line color)
- `.fm-*` — Frontmatter Card elements
- `.fmb-*` — Frontmatter Banner elements
- `.fmd-*` — Frontmatter Below elements

---

## 12. Theme Application

**Light theme (default):** `[data-theme="light"]` or unset on `<html>`
- All CSS variables resolve to light palette
- Scrollbar: dark thumb on light track

**Dark theme:** `[data-theme="dark"]` on `<html>`
- All CSS variables resolve to dark palette
- Scrollbar: light thumb on dark track

**Toggle:** `applyTheme(theme: 'light' | 'dark')` sets attribute via JavaScript at startup and on user toggle.

---

## 13. Performance Notes

### Virtualization
- **File tree:** 29px fixed row height, render ±8 rows around viewport
- **Log panel:** No virtualization (shorter, bounded list)

### Debouncing
- **Markdown rehighlight:** 320ms idle after user stops typing
- **Diagram rendering:** 200ms idle after markup change
- **History fetch:** 700ms idle after sync event
- **Save:** 650ms idle after last keystroke (unless multi-line paste)

### Animation Frames
- **DragDrop (tabs):** CSS transform + transition (dnd-kit handles)
- **Pulse:** 2.4s infinite (low visual weight)
- **Spin:** 0.7s infinite (loading indicator)

---

This spec is complete and pixel-perfect. All dimensions, colors, fonts, spacing, and interactions are exact values from the source code.

