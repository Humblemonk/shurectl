---
paths:
  - "src/ui.rs"
---

# UI look

shurectl follows Shure's MOTIV Mix desktop app in dark mode. Color values were measured from
the lossless screenshots in the MOTIV Mix user guide: shure.com/en-US/docs/guide/MOTIV-Mix
loads its HTML from `pubs2-api.prod.shureweb.eu/documents/public/render/10716.html.twig`.
Review-site JPEGs and old MOTIV builds have misled us (gray faders, blue accents); check new
colors against the guide.

**Palette.** Use the `C_*` constants at the top of `ui.rs`, never a new `Color::Rgb`. The
only literal RGBs are the MV7+ LED R/G/B channel sliders, which show the channel itself,
lightened so their bar text stays readable.

- **Lime `C_ACCENT`** marks where you are and what is active: the focused control (with a
  `BorderType::Thick` border from `focus_border()`, so focus never relies on color alone), the
  selected tab's underline, the active pill in `segmented_span()`, name editing, EQ gains,
  and the Gain and Denoiser level bars (MOTIV's fader fill). Keep it off large areas and off
  plain values: full lime frames and lime values read as too bright.
- **Values and headings** are `C_TEXT` white, labels and hints `C_DIM`, frames `C_BORDER`.
  `C_DISABLED` (3:1, below the 4.5:1 text minimum) is only for inactive things: locked tabs
  (navigation skips them), the locked gain bar, empty bar segments. Anything people read,
  including "auto" in the Summary and the meter's "no device", is `C_DIM` or brighter.
- **Bars:** Mic Level / Monitor Mix, Tone and Reverb Intensity use `C_BLUE`; Playback Level
  `C_PLAYBACK_LEVEL` prismblue, so the two adjacent monitor gauges stay distinct.
  A gauge draws its label in the bar color over the empty part and in `C_SURFACE` over the
  fill, so every bar color needs 4.5:1 against `C_SURFACE` (pure red or blue falls short).
- **States:** ON and the meter's safe zone `C_SUCCESS` (MOTIV's meter green, bluer than the
  lime), alerts `C_ERROR` (Muted ON is red, not green), warnings `C_WARN`. A locked Gain Lock
  or Config Lock block shows LOCKED, its title and its frame in `C_WARN` (a chosen state, not
  an error); Unlocked is `C_TEXT`. Red is for errors, Muted, clipping and destructive actions,
  amber for cautions; never use either as decoration.
- **Preset load box:** labels and old values `C_DIM`, each new value in its Summary row's
  colour (`summary_value()`), `C_TEXT` for values the Summary lacks, and 48V switching on in
  `C_WARN` as a caution where the Summary shows `C_SUCCESS`.

**Controls.** Option strips (Input Mode, Auto Level rows, Mic Preset, Tone Mode) go through
`segmented_span()`: gray segments with a black-on-lime active pill, pushed with no gap between
them. The selected tab is bold white over a lime underline, not a pill.

**Wording.** Hints read `[Enter] toggle` and `[Enter] cycle`. Never put "Mute" next to
ON/OFF/Enabled for anything but the mute state itself: the mute-button setting reads
"Pressing it: Works / Ignored" (`mute_btn_span()`). Card descriptions are one sentence that
wraps; don't break lines by hand.

**Size.** Every screen of every model fits 80x24 (`MIN_SIZE`, btop's minimum); below it `draw()`
shows only btop's "Terminal size too small" notice. Check a new block at 120x40 (the golden size)
and 80x24, where Main has 21 content rows and its left column 44 inner columns; the MVX2U Gen 1
Auto Main already uses all 21. Every control, value, warning and click target must show at 80x24.
Only these may be left out: focus hints, which go through `with_hint()` so they drop whole; the
status bar's full hint (it falls back to `[?] Help  [q] Quit`); the Info tab's Capabilities
list; and text whose length the user sets, a saved preset's summary line and a long device name
in the header. `every_screen_fits_the_min_size` checks the demo states with empty presets, and
compares sets of words, so a word cut in one place passes if it shows elsewhere. Dynamics
cards go through `draw_card_grid()`, which wraps them onto more rows rather than clip a title.

**Shared drawing helpers.** A control that exists on more than one model is drawn through
one of these; extend it with a parameter rather than forking a near-copy:
`draw_mode_mute_row()` (`draw_mode_block()`, `draw_mute_block()`), `draw_gain_gauge()`,
`draw_monitor_mix_gauge()`, `draw_gain_lock_block()`, `draw_config_lock_block()`,
`draw_phantom_block()`, `draw_auto_controls()`, `segmented_span()`, `draw_main_shared()`;
block styles `focus_border()`, `focus_block()`, `alert_block()`, `card_block()`; sliders
`slider_gauge()`; Dynamics cards `draw_card_grid()`; focus hints `with_hint()`; EQ
`eq_band_column()`; click targets `control_rows()`.
