# How Lua UI should look

Panels and windows built with `gpui.register_panel` / `gpui.open_window` should look like
the native panels next to them (project panel, git panel, outline, terminal). A user
shouldn't be able to tell which panels are Lua. When they differ, the Lua element
set is wrong. Don't work around it in the plugin.

Measurements below are Zed's own (`crates/ui`). Sizes are rems, so they follow the
`ui_font_size` setting. Colors are theme colors.

## Typography

| Use | Native | Size at default settings |
| --- | --- | --- |
| Body text, list item names | `Label` (`LabelSize::Default`, `text_ui`) | 14 px |
| Secondary text: branch, counts, paths next to a name, section headers | `LabelSize::Small`, `Color::Muted` | 12 px |
| Badges, keybinding hints | `LabelSize::XSmall` | 10 px |
| Dialog titles only | `LabelSize::Large` / `Headline` | 16 px |

- The UI font is used for names and labels. The buffer font (`.ZedMono`) is only for
  code, command output, hashes and diagnostics. Don't use it for paths shown as labels.
- Bare text has the same size as a native `Label` (14 px), never the window's root size.
- Sizes are named (`size = "small"`). They are not pixel numbers, which ignore
  `ui_font_size`.
- Use bold only for the item a panel is about, such as a repo name in a group header.
  Selection is shown with the row background, not by bolding the text.

## Rows and lists

- A row's children are vertically centered (`h_flex` = `items_center`). Text of
  different sizes on one row lines up on its center, so there are no superscript-looking
  paths and no metadata floating above the name.
- Clickable list rows are `ListItem`s: fixed height (`ListItemSpacing::Dense`, about 22 px
  at 14 px text), `element_hover` background on hover, `element_selected` when
  selected, and a pointer cursor. The whole row is the hit target.
- Rows have a start slot (icon or checkbox), a label that fills the row and truncates
  with an ellipsis, and an end slot (muted metadata or icon buttons, visible on hover).
- Groups are expandable with a `Disclosure` chevron icon, not "▸"/"▾" text.
  Nested rows are indented with `ListItem::indent_level`.
- Section titles are `ListHeader` / `ListSubHeader`: Small, muted, not bold, with
  actions as icon buttons on the right.

## Buttons and controls

- The default button is `ui::Button` with `ButtonStyle::Subtle` (or `Transparent`). It has no
  border and a transparent background. The background shows on hover, using
  `ghost_element_hover`. Its label is `LabelSize::Small`. Use `ButtonStyle::Filled` only for
  the single primary action of a view, such as Commit.
- Actions that are only an icon are `IconButton`s (`IconSize::Small`) with a tooltip.
  These are things like refresh, more, close, settings. Don't use unicode glyphs
  (↻ ● ☐ ☑ ▸) as icons.
- Toggles are `Checkbox` (on/off, with or without a label) or `Switch` (settings-like).
  A choice between 2–4 values is a `ToggleButtonGroup`, not buttons with a "● " prefix.
  Don't use text checkboxes like "[ ]", "[x]" or "[-]". `Checkbox` has
  `ToggleState::Indeterminate` for partially staged.
- Buttons in one view are the same size. Don't mix larger and smaller rows of buttons. If
  a view has many actions, put the main ones in the header and the rest in a
  `PopoverMenu` / `ContextMenu` behind an ellipsis icon button.
- Disabled buttons (busy, nothing selected) use `.disabled(true)`. They don't stay
  clickable just to report "busy".

## Panel layout

- Header: height `Tab::container_height` (`DynamicSpacing::Base32`, the same as the tab bar beside it), with a
  bottom border. It has the title (Default size) and optional muted context (Small),
  then icon buttons right-aligned. No "Rescan"/"Refresh" text buttons.
- The body fills the rest of the panel and scrolls as a whole with a visible scrollbar
  (`Scrollbars`). Don't use fixed pixel heights. Where a panel has two scrolling parts
  (list + output), split the space proportionally or make the split resizable.
- Footer (commit box, status line): top border, the same horizontal padding as the
  body, Small muted text. Errors use `Color::Error`.
- Spacing comes from `DynamicSpacing` (`Base02`/`Base04`/`Base06`/`Base08`): 4 px inside
  rows and 8 px panel padding at the default density. Don't pick pixel values per plugin.
- Empty, loading and error states are centered, muted, Small text, with at most one
  action button. Long errors wrap. Don't show a raw "ConnectionRefused" without saying
  what to do about it.

## Output and terminals

- Command output uses the buffer font, `editor_background` and a border. It follows the
  tail while running unless the user scrolled up. Diagnostics lines use status colors,
  and file links are underlined on hover.

## The Lua element set

`lua_hooks` (`src/ui/`) builds every element from Zed's own `ui` components, so a
plugin describes structure and gets the native look. The full reference is the module
doc in `src/ui/mod.rs`. In short:

- Structure carries the spacing: `panel{ header, footer, children }` (header at tab-bar
  height, a body that scrolls with a scrollbar, a muted footer), `section{ "Title",
  action, empty }`, `toolbar`, `split{ a, b, ratio, vertical }`, `empty{ "message" }`.
- Rows are `list_item{ "label", detail, start, end_, end_hover, selected, indent }`.
- Controls: `button` (subtle by default), `icon_button`, `checkbox`, `toggle_group`,
  `disclosure`, `icon`, `divider`, `input`.
- Sizes are named (`default small xsmall large`), spacing is a token (`none xs sm md lg
  xl`) mapped to `DynamicSpacing`, and colors are theme names. Pixel numbers still work
  but log a deprecation once.
- Props are typed (serde, `deny_unknown_fields`): a misspelled or mistyped prop is an
  error drawn in place of the element, not silently ignored.
- `col`/`row` remain for anything else; a plugin that needs them for ordinary panel
  layout is a sign the element set is missing something. Add it to `lua_hooks`.
