# Background input investigation

Target is Codex Computer Use 1001501, arm64 `SkyComputerUseService`.
SHA-256: `f21c31c0ee5df419dbd165bf76ebd65c64d864c6fec5b6c6c3a23c42ff3d2e13`.
REA signature evidence: `ev_af94b3bdaabd8fc2df405a412b0d6627e5e3d037f1870ce970ebb66e4f6f5814`.
This establishes artifact identity and its OpenAI signature, not runtime behavior.

The findings below come from the existing Ghidra project at
`/tmp/cua/gh/quick/svc`, checked against arm64 instructions. They are ordinary
Ghidra/disassembly observations, not successful REA function Evidence. REA's
fresh Ghidra import exceeded both 330 s and 900 s startup deadlines. Hopper
launch failed because macOS denied Automation permission. No client settings
were changed. Focused decompiles and live logs are in `/tmp/cua/continuation`.

## Recovered behavior

- Enforcer state is three bytes: believes active, believes focused, actually
  active. Init at `0x1007470b0`, event callback at `0x10074750c`.
  Activation events set the first two bytes together. Real front-app changes
  only update the third byte, through `0x1007515b0`.
- A temporary event filter runs before normal enforcer tracking during safe
  activation. It drops target deactivation and resigned-key/main appearance
  events. `0x10074bfc8` records activation/deactivation observations separately.
- The safe wrapper freezes updates, redirects real keys during the switch,
  restores the original front app and its window, and removes temporary filters.
  Poll interval is 10 ms, activation retry 50 ms, quiet interval 100 ms, deadline
  2 s. Entry `0x100748370`, continuations `0x1007498ec`, `0x10074a96c`.
- System focus protection watches process notifications, independently of app
  activation. Taken is dropped, Changed releases the recorded focus ID once,
  and one matching Returned is dropped. Real keys addressed to the thief go to
  the prior owner; self-posted keys pass. Source functions are in
  `/tmp/cua/gh/FSP`; original behavioral specification is
  `/tmp/cua/spec/focus_steal_preventer.md`.
- The global click watcher lets the user activate a synthetically active app
  normally. It passes the click after activation, rather than dropping it.
  Callback `0x100751108`.
- Normal element clicks use frame center, not AXActivationPoint.
  `0x1007a508c` gets the frame through `0x1007a6838` and returns its midpoint.
  The full original also checks ancestor clipping and has a Chess-specific
  point search. Those paths are not yet ported.
- For a visible web element, the original hit-tests its click point and searches
  the hit element's ancestors for the requested element. A match selects mouse
  input instead of AXPress. `0x1007bee04`, `0x1007beefc`, `0x1007be664` and
  `0x1007b8190` establish that path.
- Selection-sensitive clicks check the element and its ancestors, up to 300
  levels, for settable AXSelected or supported AXSelectedRows/Cells/Children.
  Menu items and menu bar items do not count as individually selectable.
  `0x1007ae150`, `0x100795630`, `0x1007b67f0`.
- Synthesized events receive a fresh uptime timestamp immediately before posting.
  `0x10075ad0c`.

## Live checks and limits

The isolated Chrome page is `/tmp/cua/continuation/input.html`. It has a named
input and a click counter. The original implementation passed the sequence
Cmd+L, address typing, Escape, click the page field, type into that field.
`original.log` records the exact field value. Finder remained frontmost in
`focus-original.log`.

The first Rust assertion was too broad: it accepted text anywhere in the AX
response, including the omnibox. It is not evidence of correct field typing.
The stricter regression failed before the frame-center fix. Its event trace
showed a click at `(0,1329)`. A separate read-only AX probe confirmed Chrome
reported that bogus AXActivationPoint while the input's actual frame was
`(2143,1584,278,64)`. The frame-center fix has built but still needs its live
regression run.

Concurrent relay restarts also produced competing service instances. The service
now holds an exclusive process lock before replacing the socket. Relaunch and
session-teardown checks remain part of the next live batch.

Remaining fidelity work includes menu-dismissal suppression, the ViewBridge to
ViewBridge special release/notification path, exact exceptional host resolution,
ancestor clipping and the Chess-specific click-point search. Screen-update
suppression and safe-wrapper completion still need a visual check. Full parity
must not be claimed until those paths are resolved or explicitly scoped out.

## Next live batch

Only run after the user says they have stepped away. Keep one service instance.
Repeat the strict Chrome field test, then coordinate clicks, Unicode text,
synthetic Escape, native TextEdit, focus changes between actions, and session
cleanup. Capture front/key-focus observations alongside a screen recording.
Close only test windows afterward. End the turn before requesting the user's
next away period.

## Live batch, 2026-10-24 (main at 8faf5d6)

Test page `/tmp/cua-test/input.html` in background Chrome, with the front app
logged every 20 ms and the screen recorded.

- Element click on the page field, then typing: field value is exactly the typed
  text. Coordinate click on a button: its counter went from 0 to 1.
- Cmd+L, Unicode text (`üñí 日本 ✓`), synthetic Escape: all reached Chrome and
  the session survived the Escape.
- A second Cmd+L run kept the user's app frontmost the whole time. The
  recording shows no Chrome or TextEdit frames during it.
- TextEdit: typing and Cmd+A work. "native ok" became "Native ok" after Cmd+A;
  probably system auto-capitalization, not confirmed.
- Window screenshots of Chrome are blank white while the omnibox popup is open.
  `screencapture -l` on the main window returns white too, so window selection
  is not the cause.

## Recovered and ported, 2026-10-24 (continued)

Persistent Ghidra project at `/tmp/cua-re/gh/proj` (import about 90 min,
then about 5 s per function with `Dec.java`). REA CLI evidence for the import:
`ev_36a8f102c387a356ed92154cddf7cac08c0e772d2ddb430b79c6d7f81ae2fade`.

- ViewBridge host (`0x100759300`): polls only when the last synthesized action
  was under 1 s ago. That time is written only by
  `synthesizedActionWasPerformed` (`0x10074c9f4`), which has no callers, so the
  agent-host resolver (`0x100757e70`) always runs. Our `classify` matches it.
- ViewBridge-to-ViewBridge focus change (`0x1007552d0`): see commit e8a30ac.
  `UIElement.ProcessType` (`0x100794e84`): 0 launched regular app, 1 launched
  prohibited app, 2 otherwise. `containingElement` (`0x100794b38`) walks the
  parent chain including self, up to 300 levels.
- `clickablePoint()` (`0x1007a506c`): frame midpoint clipped by scroll areas,
  scrolling groups, and the enclosing window, sheet, menu or popover. Nil makes
  a mouse click throw `cannotClickOffscreenElement` (SynthesizedEvent.Error 3).
- `scrollToVisible(frame:)` (`0x1007a5d40`, `0x1007a5dd0`, `0x1007a7298`):
  ported in commit fa8ec42. Scroll-bar arithmetic is a best reading.
- Chess (`0x1007a54e0`): AXButton in `com.apple.Chess` whose center misses it;
  centroid of exact hits on a 64x64 grid. Unresolved: the string test in
  `0x1007a9ee0` (first character from a set built at run time, second in
  `1`..`8`, likely a board square name) and `0x10075e4dc`.
