# Rust Computer Use

## Install

Apple silicon, macOS 14.4 or later. Download the latest release and register it with your MCP client:

```sh
gh release download v0.0.1 -R yspreen/rust-computer-use
shasum -a 256 -c rust-computer-use-v0.0.1-aarch64-apple-darwin.tar.gz.sha256
tar -xzf rust-computer-use-v0.0.1-aarch64-apple-darwin.tar.gz
xattr -c rust-computer-use-v0.0.1-aarch64-apple-darwin/rust-computer-use
claude mcp add computer-use -- "$PWD/rust-computer-use-v0.0.1-aarch64-apple-darwin/rust-computer-use"
```

The binary is ad-hoc signed, not notarized. A browser download gets a quarantine flag
that stops it from running. The `xattr` line clears it, and does nothing if the flag
isn't there. Releases are listed on the
[releases page](https://github.com/yspreen/rust-computer-use/releases).

To build from source instead:

```sh
cargo build --release
claude mcp add computer-use -- "$PWD/target/release/rust-computer-use"
```

## What it is

Clean-room Rust MCP stdio server with the same tool interface as OpenAI's Codex
Computer Use (`list_apps`, `get_app_state`, `click`, `drag`,
`perform_secondary_action`, `press_key`, `scroll`, `select_text`, `set_value`,
`type_text`). The target app is never activated. Input goes to its process and
window, so your frontmost app, cursor and keyboard focus stay put.

It needs Accessibility and Screen Recording. macOS grants both to the app that
launches it (your terminal, editor or agent app), not to this binary. On the first
call without them, a setup window opens and lists both permissions. Allow opens the
right System Settings pane, and a tile with the host app's icon docks under that
window. Drag the tile into the list and switch it on. Run
`rust-computer-use --setup` to open the window yourself. It sends no Apple Events,
so you won't get per-app Automation prompts.

## Safety

- **Approval per app.** The first `get_app_state` for an app asks you through MCP
  elicitation ("Allow Rust Computer Use to use Calculator?"). Approvals are saved in
  `~/Library/Application Support/rust-computer-use/approvals.json`. Delete an entry to be
  asked again. Clients without elicitation are refused unless
  `RUST_COMPUTER_USE_APPROVE_ALL=1` is set.
- **Blocked apps.** Terminals (Terminal, iTerm2, Ghostty, kitty, WezTerm, Warp,
  Rio, Wave) and OpenAI's own apps are refused, the same list the original uses.
- **Esc stops the agent.** While the agent is working, a pointer shows where it
  acts. Pressing Esc makes every further call return the original's "stopped by
  the user" message, until the agent has been idle for 30 seconds.
- **Locked screen.** Nothing is read while the Mac is locked.

## How it works

- **Tree:** AXUIElement walk of the key window plus the menu bar, in the original
  text format. That covers flags such as `(selected, settable, boolean)`, fields such as
  `Description`, `URL`, `Value`, `Placeholder` and `Secondary Actions`, and a closing
  line naming the focused element. Bare one-child containers collapse, and empty
  ones are dropped.
- **Text values:** styled text renders as Markdown (`**bold**`, `*italic*`,
  `<u>`, `~~`, `[link](url)`), built from `AXAttributedStringForRange`.
  `select_text` matches that Markdown and maps the match back to plain offsets.
- **Chromium/Electron:** `AXManualAccessibility` (Electron) and
  `AXEnhancedUserInterface` (Chrome) switch on the web tree. A nearly empty tree is
  re-read twice, because Chromium builds it lazily.
- **Tables:** only on-screen rows, noted as `(showing 0-58 of 100 items)`. A row
  reads as its cells' text, and outline rows get `Collapse`/`Expand`. Runs of
  plain adjacent labels merge into one `text` line. When rows are selected, a
  `Selected:` section and a note to the agent close the tree.
- **Menus, sheets, hidden and minimized windows:** while a menu is open, the
  state is that menu, rooted at its menu bar item, with no screenshot. The
  original does the same. Close it with the item's `Cancel` action. An open
  sheet becomes the root, and an untitled window prints `Window: "Unknown"`.
  Minimized windows are restored, and hidden apps stay hidden but still get a
  screenshot, both without activating the app. When the focused field has
  selected text, a `Selected text:` block ends the tree.
- **Screenshot:** `screencapture -l<windowID>`, JPEG, at full resolution as the original sends it.
- **Clicks:** `AXPress` when available. Otherwise `CGEventPostToPid`, addressed to
  the window (see below).
- **Keys:** characters map to keycodes through the current keyboard layout
  (`UCKeyTranslate`). Only a US layout has been tested. Text goes in through `AXSelectedText`
  when the app actually applies it. Otherwise it is sent as key events.
- **Scroll:** `AXScroll*ByPage` for whole pages. Otherwise pixel-unit wheel
  events, where a page is the element's height or width.
- **Actions:** each action returns the resulting state and screenshot. An action
  is refused if the app changed since the last `get_app_state`, as the original does.
  Before an app's first `get_app_state`, actions get the original's "not active"
  message. Errors use its wording and `Computer Use server error -10005:` prefix.
- **Mouse timing:** mouse events are ~50 ms apart. SwiftUI ignores presses whose
  down and up arrive within a few milliseconds.
- **list_apps:** NSWorkspace running apps, plus the original's Spotlight query
  (`kMDItemLastUsedDate_Ranking >= $time.today(-14)`, `kMDItemUseCount`). Like
  the original, it uses UTC dates and skips apps with a boolean `LSUIElement`.
  It also hides recent apps whose bundle ID is a prefix of a running app's. The
  output matched the original line for line on this Mac.
- **Per-app instructions:** set `RUST_COMPUTER_USE_INSTRUCTIONS` to a directory of
  `<App name>.md` or `<bundle id>.md` files. The original bundles its own,
  and they are not copied here.
- **`app`** also accepts a pid, which picks one instance when several share a
  bundle ID.

## Findings from the original (26.1013.1001501)

- The service resolves every CGEvent and SkyLight function through `dlsym`, and
  the names never appear in the binary. SIP blocks attaching at runtime.
- `CGEventTapCreateForPid` on the target app captures what the service sends.
  Mouse events carry the window number in fields 51, 91 and 92, plus a
  window-local location (`CGEventSetWindowLocation`). AppKit drops background
  clicks without them. Keyboard events need nothing special.

## Verified in the background

Calculator (SwiftUI), TextEdit (AppKit), Disk Utility (outline), Chrome web
content, and Claude desktop (Electron). Each was tested with the target app
behind another app, and the frontmost app was checked before and after. The
trees for Calculator, TextEdit and Disk Utility match the original's output line
for line. Esc-to-stop has not been tested with a real key press.

## Known differences

- The original's built-in browser guidance and bundled per-app instructions
  are not reproduced. Supply your own through `RUST_COMPUTER_USE_INSTRUCTIONS`.
- The `Window:` line uses the AX title untruncated. The original shortens
  long titles.
- An app can refuse an action while in the background. Calculator rejects
  `AXRaise` with AXError -25205.
- The original draws its target window as active (key) without activating the app.
  Rust Computer Use leaves the window's appearance alone.
- Not reproduced: the original's URL blocklist (fetched from OpenAI), the
  lock-screen auto-unlock helper, and its other non-tool features (appshots,
  recording, Messages).
