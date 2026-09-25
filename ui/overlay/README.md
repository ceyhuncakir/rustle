# Overlay pill

The floating island that shows what dictation is doing: a port of the GNOME
Shell version in `extension/` onto one `<div>` and one `<canvas>`, for the
platforms where Rustle draws its own always-on-top window.

`island.ts` owns the states, sizes and animations; `../shared/visualizer.ts`
paints the bars, dots, check and cross; `../shared/theme.ts` holds every
colour, width and duration, copied from `extension/theme.js`. The command and
event contract with the native side is in `../shared/api.ts` (`overlay_ready`,
`overlay_resize`, `rustle:state`, `rustle:text`, `rustle:level`). `overlay_resize`
reports the pill's own box, border included; the page centres the pill, so any
margin the native side adds for the shadow (30 px to each side, 40 px below)
stays symmetric.

## Preview in a browser

```sh
pnpm dev
```

Open <http://localhost:1420/overlay/index.html>. Without the native app the
page falls back to `ui/shared/mock.ts`, which cycles the pill through
idle → listening → thinking → inserting → hidden → … → error on a timer and
pushes random microphone levels while it is listening. The page background is
transparent, so set a dark backdrop in devtools to judge the shadow.

To pin one state for a screenshot, pass it in the query string:

```
/overlay/index.html?state=listening
/overlay/index.html?state=inserting&text=The%20meeting%20moved%20to%20four
/overlay/index.html?state=error&text=Ollama%20is%20not%20running
```

`state` is one of `idle`, `listening`, `thinking`, `inserting`, `error`,
`hidden`; `text` is optional and switches the pill to its two-line, 66 px
form. `inserting` self-hides after 1.4 s, as it does in the real thing.
`window.island` is exposed, so `island.setState("error")` and
`island.setText("…")` work from the console too.
