# Rust Computer Use

## Install

Apple silicon, macOS 14.4 or later. Download the latest release and register it with your MCP client:

```sh
base=https://github.com/yspreen/rust-computer-use/releases/latest/download
curl -fLO "$base/rust-computer-use-aarch64-apple-darwin.tar.gz"
curl -fLO "$base/rust-computer-use-aarch64-apple-darwin.tar.gz.sha256"
shasum -a 256 -c rust-computer-use-aarch64-apple-darwin.tar.gz.sha256
tar -xzf rust-computer-use-aarch64-apple-darwin.tar.gz
xattr -cr "Rust Computer Use.app"
mv "Rust Computer Use.app" /Applications/
claude mcp add rust-computer-use -- "/Applications/Rust Computer Use.app/Contents/MacOS/rust-computer-use"
```

The app is signed but not notarized. A browser download gets a quarantine flag
that stops it from running, and the `xattr` line clears it. Claude Code reserves
the name `computer-use`, so register it under another name. Releases are listed
on the [releases page](https://github.com/yspreen/rust-computer-use/releases).

To build from source instead:

```sh
scripts/bundle.sh   # set CODESIGN_IDENTITY to sign with your own certificate
claude mcp add rust-computer-use -- "$PWD/target/release/Rust Computer Use.app/Contents/MacOS/rust-computer-use"
```

## What it is

A clean-room Rust MCP server with the same tool interface and output as OpenAI's
Codex Computer Use (`list_apps`, `get_app_state`, `click`,
`perform_secondary_action`, `set_value`, `select_text`, `scroll`, `drag`,
`press_key`, `type_text`). The target app is never activated. Input goes to its
process and window, so your frontmost app, your cursor and your keyboard focus
stay where they are. Most of the behavior below was read out of the original's
binary with Ghidra and checked against event taps on the apps it drives.

## Permissions

It needs Accessibility and Screen Recording, and asks for them under its own
name. The binary your MCP client starts only relays to the app, which macOS
starts the way it starts any app. Permissions then belong to "Rust Computer
Use", not to your terminal or agent app. The original works the same way.

On the first call without them, a window opens with a card per permission.
Allow opens the right System Settings pane and docks a small panel in its
corner with a tile of the app. Drag the tile into the list and switch it on. You
won't see a system prompt, and the app sends no Apple Events, so no "wants to
control" Automation prompts either. Open the app from Finder, or run the binary
with `--setup`, to bring the window up yourself.

Grants are tied to the code signature. Builds signed ad hoc get a new one each
time, so sign with a stable certificate (`CODESIGN_IDENTITY`) if you rebuild often.

## The cursor

While the agent works, a cursor shows where it acts. It is the original's
shape, and it moves the way the original's does. Long moves follow the best of 20
candidate curves, timed by a spring, and short hops stretch and tilt. The click
goes out once the cursor is close enough, and the cursor dips when it presses.
It sits just above the app it works in, so windows covering that app cover the
cursor too. When the app's window isn't on screen (another Space, or a hidden
AeroSpace workspace) it skips the animation. It fades away after a few idle
seconds. Your real pointer never moves.

## Safety

- **Any app, no prompts.** Every app is allowed without asking, and nothing is
  blocked. Terminals, password managers and browsers are all fair game.
- **Esc stops the agent.** Every further call returns the original's "stopped by
  the user" message, until the agent has been idle for 30 seconds.
- **Locked screen.** Nothing is read while the Mac is locked.

## How it works

- **Tree:** the original's transform pipeline over the key window plus the menu
  bar. Single-child groups merge into their child, repeated hierarchy flattens,
  fields that repeat an earlier value are dropped, and lines follow its field and
  flag order. Tables print every row up to 500. On the seven apps compared
  (TextEdit, Calculator, Safari, Chrome, Finder, System Settings, Preview), the
  output matches the original's line for line.
- **Window title:** the window server's name for the window, then the AX title.
- **Chromium and Electron:** `AXManualAccessibility` when the app lists it, else
  `AXEnhancedUserInterface`, written on every read because Chrome can drop its
  web tree while still reporting the flag on. The flag is switched back off when
  the session ends.
- **Background apps:** before the first synthesized event, the app gets the
  AppKit "application activated" event for its window and a press at the
  window's activation point. That's the original's trick. Without it, shortcuts
  such as `super+s` do nothing in a background app.
- **Clicks:** `AXPress` for plain left clicks on elements, else mouse events at
  the element's activation point. Mouse events carry the window number, event
  numbers, pressure and subtype the original sets.
- **Keys and text:** modifiers are sent as flagsChanged events around the key.
  `type_text` sends key events from the current layout across eight
  shift/option/control combinations. Characters no key produces are sent as
  unicode events rather than dropped.
- **Scroll:** one continuous pixel event of pages times the element's height or
  width.
- **After an action:** the state is read once the app's AX notifications have
  been quiet for 0.25 s, at most 5 s.
- **Screenshot:** `screencapture -l<windowID>`, JPEG, at full resolution. It works
  for windows on other Spaces too.
- **App instructions:** the original's browser note for any app that handles
  http links. Per-app notes come from the Codex Computer Use app's bundled
  AppInstructions if it is installed.
  Each is sent once per session.
- **`app`** also accepts a pid, which picks one instance when several share a
  bundle ID.

## Known differences

- The original's fog cursor blurs whatever is behind it. This one draws a soft
  shadow instead.
- Not reproduced: the URL blocklist (fetched from OpenAI), the lock-screen
  unlock helper, and the original's non-tool features (appshots, recording,
  Messages).
