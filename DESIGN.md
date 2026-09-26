# Getting Settings to the design

The source is the Settings mockup — the same design language as Files'
(`crates/hyprforge-files/DESIGN.md`), with two candidate shells (`1a` solid,
`1b` compositor) and seven supporting screens: Displays, Windows &
workspaces, Network, Sound, Keybinds, the display keep/revert prompt, a
global search palette, and Power & battery.

**Shell: `1b`.** The mockup settles it — everything from `1c` on is drawn in
`1b` chrome — and Files already uses it, so the two apps share one frame.

## The rules, the same as Files'

The mockup is drawn in Dracula hex values. None of them appear in the code:
every colour comes from `hyprforge_look::Theme`, resolved from settings the
user already controls, per CLAUDE.md's "Never add a colour constant to an
app".

| mockup | role | Theme |
|---|---|---|
| four greys | elevations | `surfaces.{sidebar,root,card,row}`, plus `card_border` |
| alternating row stripe | | `row_tint()` — `card_border` at 22%, not a fifth grey |
| purple | selection, and "on" | `accent` |
| green / orange / red / cyan | state only | `success` / `warning` / `error` / `info` |
| IBM Plex Sans / JetBrains Mono | text / config | the desktop's own fonts from gsettings; `mono_font` is `monospace-font-name`, and only when fontconfig actually has it |

Where the mockup spends a colour on something that is not a state, it loses
it: per-page sidebar icons are one dim colour, accent only when selected; a
setting's value in the search palette is plain text, not green for true and
red for false; a conflicting keybind is the warning colour the Theme gives a
conflicting bind, not red.

## What was built

Every sizing is a ratio of the Theme at 100% scale (`hyprforge-ui`'s
`density`), so a larger font grows rows rather than clipping them.

- **Shared widgets** (`hyprforge-ui/src/widgets/`): switch, value and stepped
  sliders, segmented choice, inset field and dropdown styles, chips, keycaps,
  config lines, striped setting rows, page header, hero card, pending bar,
  countdown ring; drawn page, signal and battery marks (`glyph.rs`).
- **The shell** (`src/main.rs`): header bar with the search field and a
  live/pending chip, the sidebar in the mockup's four groups (System,
  Connectivity, Hyprland, Personal), page headers drawn by the shell, one
  scroll area, one pending-changes bar for every page, and the search palette
  over every page, setting and config key.
- **Pages**: the catalogue pages as striped rows with each setting's config
  line; Displays' arrangement canvas and rows; the keep/revert countdown;
  Keybinds as a table and its editor; Windows & workspaces' Writes block;
  Network, Power & battery, Bluetooth, Tray and Window rules as hero cards and
  striped groups; Idle & lock, Wallpaper, Night light, Screen sharing and
  Session with their lists as entry blocks (a title, a switch where the entry
  can be off, a quiet Remove, its fields as striped rows); Default apps,
  Appearance and Animations as striped rows. Every on/off is a switch. The
  only checkboxes left are the import reviews' "include this", which is what
  a checkbox is for. A few forms — the Window rules editor, Displays'
  Advanced section — still lay out through `row_field`, so their rows are
  the new shape but unstriped.

## Departures from the mockup

- **Session keeps its name**, not "Autostart & environment": the page also
  holds gestures and permissions.
- **No Sound, Users or Notifications entries**: there is no module behind
  them, and an entry that opens onto nothing is a promise the app does not
  keep.
- **Windows & workspaces' config block sits above the rows**, not beside
  them: the content column is 880px, and a second column would squeeze both.
- **The Wi-Fi card has no Disconnect**: the module has no Wi-Fi disconnect
  action.
- **Power profiles say what power-profiles-daemon does**, not the mockup's
  "60 Hz, no blur".
- **No sidebar footer with the config path**: each page writes its own TOML
  and generated Lua, so one path there would be false for most pages. The
  header's live/pending chip says whether anything is unwritten.

## Deferred — each needs a backend, not a restyle

- The Sound page (PipeWire devices, meters, per-app mixer).
- Network's IP/DNS detail rail and throughput graph.
- Power's 24-hour chart and charge limit.
- The live tiling preview on Windows & workspaces.
- Displays' VRR and 10-bit rows — the display model has neither.
- The keep/revert prompt's diff of the monitor lines and backup path —
  `RevertPending` carries only the seconds.
- The palette's "space toggles inline".

## Checking it

Screenshots, on the nested compositor, with the test copy given its own
short `XDG_RUNTIME_DIR` and an isolated `XDG_CONFIG_HOME` — see the README's
"It writes real configuration" section for why both. `--screen <name>` and
`--search <text>` reach any page or the palette. Displays' canvas and the
revert prompt need two monitors, from `hyprforge-displayd run --mock` on the
private bus in `testing/private-bus.conf`.
