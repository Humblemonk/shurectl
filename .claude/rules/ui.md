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
only literal RGBs are the MV7+ LED R/G/B channel sliders, which show the channel itself.

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
- **States:** ON and the meter's safe zone `C_SUCCESS` (MOTIV's meter green, bluer than the
  lime), alerts `C_ERROR` (Muted ON is red, not green), warnings `C_WARN`. A locked Gain Lock
  or Config Lock block shows LOCKED, its title and its frame in `C_WARN` (a chosen state, not
  an error); Unlocked is `C_TEXT`. Red is for errors, Muted, clipping and destructive actions,
  amber for cautions; never use either as decoration.

**Controls.** Option strips (Input Mode, Auto Level rows, Mic Preset, Tone Mode) go through
`segmented_span()`: gray segments with a black-on-lime active pill, pushed with no gap between
them. The selected tab is bold white over a lime underline, not a pill.

**Wording.** Hints read `[Enter] toggle` and `[Enter] cycle`. Never put "Mute" next to
ON/OFF/Enabled for anything but the mute state itself: the mute-button setting reads
"Pressing it: Works / Ignored" (`mute_btn_span()`). Card descriptions are one sentence that
wraps; don't break lines by hand.

**Size.** Check a new block at 120x40 (the golden size) and 80x24. Main's left column has fixed
block heights, and the MVX2U Gen 2 and MV6 Gen 2 Manual Main already need 23 rows of the 18
that 80x24 leaves, so it squashes blocks there.
