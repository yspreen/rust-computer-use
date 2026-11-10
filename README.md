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
claude mcp add -s user rust-computer-use -- "/Applications/Rust Computer Use.app/Contents/MacOS/rust-computer-use"
```

The app is signed but not notarized. A browser download gets a quarantine flag
that stops it from running, and the `xattr` line clears it. Claude Code reserves
the name `computer-use`, so register it under another name. Releases are listed
on the [releases page](https://github.com/yspreen/rust-computer-use/releases).

To build from source instead:

```sh
scripts/bundle.sh   # set CODESIGN_IDENTITY to sign with your own certificate
claude mcp add -s user rust-computer-use -- "$PWD/target/release/Rust Computer Use.app/Contents/MacOS/rust-computer-use"
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

Grants are tied to the code signature. Releases from v0.0.4 on are all signed with
the same certificate, so updating keeps them. Your own builds are signed ad hoc,
which changes every build, unless you pass a certificate (`CODESIGN_IDENTITY`).

## The cursor

While the agent works, a cursor shows where it acts. It is the original's
shape, and it moves the way the original's does. Long moves follow the best of 20
candidate curves, timed by a spring, and short hops stretch and tilt. The cursor
dips when it gets there. Unlike the original's, it only shows the action: the
click goes out at once, without waiting for the cursor.
It sits just above the app it works in, so windows covering that app cover the
cursor too. When the app's window isn't on screen (another Space, or a hidden
AeroSpace workspace) it skips the animation. It fades away after a few idle
seconds. Your real pointer never moves.

## Safety

- **Any app, no prompts.** Every app is allowed without asking, and nothing is
  blocked. Terminals, password managers and browsers are all fair game.
- **Esc doesn't stop the agent.** The original stops every session when Esc is
  pressed anywhere during one, so an Esc meant for another app stopped agents
  working in parallel. This one never stops on a key.
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
  web tree while still reporting the flag on. The app's role is read first:
  Chromium then builds the page's tree at once instead of 2 s later. The flag is
  switched back off when the session ends.
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
- **Open and save panels:** in sandboxed apps (Safari's file upload, TextEdit's
  Open) an XPC service the app is responsible for draws the panel, in a hidden
  copy of the app's window. Keys go to that service, as the original sends them.
  Keys sent to the app itself only beep.
- **Scroll:** one continuous pixel event of pages times the element's visible
  height or width.
- **After an action:** the app gets a flagsChanged event with no modifiers, so
  AppKit finishes what it does at the end of an event (TextEdit deadlocks in a
  later save otherwise). Then the state is read once the app's AX notifications
  have been quiet for 0.25 s, at most 5 s.
- **Hung apps:** every call first asks the app for its role, with a 2 s timeout.
  An app that doesn't answer fails the call with `appNotResponding`.
- **Screenshot:** the window's own pixels (`CGWindowListCreateImage`), encoded
  as JPEG in the service, at full resolution up to 2000 px on the long edge. It
  takes about 15 ms, where `screencapture` takes 130, and works for windows that
  are covered or off-screen. `screencapture -l<windowID>` is the fallback.
- **`app`** also accepts a pid, which picks one instance when several share a
  bundle ID.

## A browser window per session

`open_browser_window` (not in the original) opens a Safari window for the
session. Safari is the agents' browser, left alone by the user, so logins made in
it persist for later sessions. The session that opened the window gets it
whenever it passes `app: "Safari"`; other sessions never see it and get another
Safari window instead. It closes when the session ends or after 10 minutes
unused, and a restarted service closes the ones an earlier one left open. A sheet
left open on the window (a site asking for the location) is cancelled so the
window can close, and a leave-this-page prompt is confirmed. When
AeroSpace runs, the window moves to a hidden workspace, `agents`, right after it
opens. Subagents share their parent's session, so each passes its own `name`
and then uses `app: "Safari#<name>"`.

Actions that load a page (Return, a submitted address bar, a followed link)
answer with the loaded page, waiting up to 10 s for it.

Safari has one key window, and keys go to it. Before each action the session's
window is made key inside Safari, without activating Safari. Once the app has
settled (it takes in queued keys only then), the first window no agent owns is
made key again. Should the user be in Safari, that
would pull their focus away (and AeroSpace would follow it to the hidden
workspace), so then only actions that need no focus run: reading state, clicking
elements by index, `set_value` and scroll. An address bar is submitted with its
accessibility action instead of Return, so navigating works too. Keys, typing,
mouse clicks and other submitting fields ask the agent to wait.

The user's own input to Safari (scrolling over their window, typing in it) never
holds up a session working in its own window, which takes no user input.

## Known differences

- The original's fog cursor blurs whatever is behind it. This one draws a soft
  shadow instead.
- `set_value` on a Finder file name fails, and puts the real name back on screen,
  when the file wasn't renamed. The original reports success.
- A page of `scroll` is what's visible of the element. The original uses the
  element's whole frame, which for a web area is the whole document, so 0.25
  pages of a long page jumped a third of the way down.
- Calls sent together (several `set_value`s from one `get_app_state`) resolve
  element indices and pixels against the state the client had when it sent
  them. In the original the first call's result renumbers the tree, and the
  next call hits a different element.
- `set_value` on a pop-up button (a native `<select>`) picks the option with
  that title and checks that the button shows it. The original refuses, as the
  button isn't settable. Clicking an item in a pop-up's menu that doesn't change
  the pop-up is an error, where the original reports success.
- Two places take no synthesized mouse events: windows off every display (a
  window manager's hidden workspace), where AppKit hit-tests them at the
  display's edge, and sandboxed apps' open and save panels, drawn by another
  process. Clicks there, by element or by pixel, go through accessibility: a row
  gets selected (a double click then opens it with Return), a field gets focus,
  and anything else is an error instead of a click somewhere else. Off-display
  scrolls move the scroll bar. Web content is exempt, as WebKit still takes
  those events. The click tool's description and the off-screen note name these
  fallbacks as a second choice after element actions.
- When [AeroSpace](https://github.com/nikitabobko/AeroSpace) is running, a pixel
  click on an off-display window that the accessibility fallback can't do
  switches AeroSpace off through its socket, clicks where the window came on
  screen, and switches it back on (about 1.3 s; every hidden window shows
  briefly). Workspace assignments stay as they were. Without AeroSpace, nothing
  of this is advertised or done.
- Not reproduced: the URL blocklist (fetched from OpenAI), the lock-screen
  unlock helper, and the original's non-tool features (appshots, recording,
  Messages).
