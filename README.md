# ClaudeUsage

![ClaudeUsage](etc/usagemonitor.jpg)

A cross-platform tray app showing your Claude subscription usage — the 5-hour
session window and the 7-day weekly window — in your menu bar or system tray,
with a popover on click.

**macOS, Windows and Linux** (Ubuntu, Debian, Arch, Bazzite). Built with Rust
and Tauri v2; ~8.8 MB.

---

## ⚠️ Terms-of-Service notice — read this first

**This app is for personal use only and is not compliant with Anthropic's
Consumer Terms of Service. Do not distribute or share it.**

It works by reusing the OAuth token that Claude Code stores on your machine and
calling the Anthropic API on your behalf to read usage. Anthropic's policy is
explicit that subscription (Free/Pro/Max/Team/Enterprise) OAuth credentials are
"intended exclusively… to support ordinary use of Claude Code and other native
Anthropic applications," and that third-party apps may not route requests
through those credentials. There is **no sanctioned API** for subscription
usage data (a Console API key bills separately and does not expose your Pro/Max
quota), so there is no compliant way to build exactly this.

Practical implications:
- Use it only on your own machine, with your own account. Never ship it.
- Anthropic began technically rejecting subscription OAuth tokens from
  non–Claude-Code clients in early 2026; this app may stop working at any time.
- If you want this functionality on firm footing, ask Anthropic
  (contact sales) about a permitted method, or use Claude Code's own `/usage`.

This is documented honestly rather than hidden. Proceed at your own discretion.

---

## How it works

- **Auth:** reuses the OAuth credential Claude Code already stores — the login
  keychain on macOS, `.credentials.json` on Linux and Windows. No in-app login.
  The app holds no credential of its own and never stores or transmits the
  token anywhere except the Anthropic API request. See
  [docs/auth.md](docs/auth.md).
- **Per device:** it shows the usage of whatever account is signed into
  `claude` (Claude Code) on that machine.
- **Fetch:** every 60 s (configurable) it sends a minimal 1-token
  `POST /v1/messages` request (`anthropic-beta: oauth-2025-04-20`) and reads
  usage from the response headers:
  `anthropic-ratelimit-unified-5h-utilization` / `-7d-utilization` (fractions)
  and the matching `-5h-reset` / `-7d-reset` epochs. This is the same technique
  established Claude usage monitors use, and is more durable than the
  undocumented `/api/oauth/usage` endpoint.
- **Tray display:** both percentages are rendered into the icon bitmap, because
  a menu-bar item clips multiline text and Windows has no tray text at all.
  Colour follows the usage threshold — green below 70 %, amber below 90 %, red
  above. Settings offers other layouts: two icons (one per metric), one icon
  alternating between them, or a single metric. Linux puts the numbers in the
  tray title instead.
- **Popover:** opens under the icon you clicked, on whichever screen that is,
  including mixed Retina and non-Retina setups. It shows both bars, an
  optional 24 h / 7 d usage history, and Claude service status. When a service
  is not operational, the collapsed status row names it, e.g.
  `Degraded (claude.ai)`.
- **Menu bar only on macOS:** no Dock icon, including when started at login
  (`LSUIElement` in `crates/app/Info.plist`).
- **Unsandboxed** on macOS so it can read Claude Code's login-keychain item.

---

## Project structure

```
ClaudeUsage/
├── Makefile                  every build/check command; `make` lists them
├── Cargo.toml                workspace — the only place the version lives
│
├── crates/
│   ├── core/                 platform-agnostic logic, no UI, no OS calls
│   │   └── src/              credentials, usage, status, model, alerts
│   └── app/                  the Tauri host
│       ├── src/              main, state, settings, history, autostart,
│       │                     tray/{render,sizing,placement,linux}
│       └── Info.plist        merged into the macOS bundle (no Dock icon)
│
├── ui/                       popover: index.html, style.css, app.js
├── packaging/                Linux container build (.deb/.rpm/.AppImage)
└── docs/
    ├── architecture.md       how it works, and why — start here
    ├── auth.md               credential sources and the macOS chain
    └── cross-platform.md     the port's record and its measurements
```

---

## Build & run

`make` lists every task. The common ones:

```bash
make run        # run the app
make check      # fmt + clippy + tests
make bundle     # macOS .app and .dmg
make poll       # one live usage poll, no GUI
make probe      # which credential source resolves
```

Requires rustc 1.88+ (Tauri's floor). `rust-toolchain.toml` pins it; if
Homebrew's older rust shadows rustup on your `PATH`, the Makefile resolves
around that for you.

**First launch after installing the DMG** — the build is unsigned, so clear the
quarantine flag. Recent macOS `xattr` has no `-r`, hence `find`:

```bash
find /Applications/ClaudeUsage.app -exec xattr -d com.apple.quarantine {} \; 2>/dev/null
```

**Updating an installed build with Launch at login on** — launchd pins the
login item to the exact binary, and an unsigned build changes identity with
every rebuild, so the new copy is refused (`OS_REASON_CODESIGNING`) until the
item is reloaded. Quit the app, replace it in `/Applications`, then:

```bash
launchctl bootout   gui/$(id -u)/ClaudeUsage
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/ClaudeUsage.plist   # also starts it
```

Settings and usage history live in `~/Library/Application Support/ClaudeUsage`
and survive reinstalls.

### Other platforms

```bash
make linux-check    # compile, lint and test for Linux in Docker
make linux-bundle   # .deb, .rpm and .AppImage
make windows-check  # type-check the Windows-only code paths
```

Linux needs `libwebkit2gtk-4.1` and `libayatana-appindicator3`; the bundles
declare them. On GNOME — which Bazzite ships — the tray needs the AppIndicator
extension, and the app says so on startup if it is missing. **AppImage is the
recommended artifact for Bazzite**, since layering a package on an immutable OS
means a reboot.

> Windows has been run on real hardware; on Windows the popover is an ordinary
> window that remembers where you put it. Linux is written and machine-checked
> but **has not been run on real hardware yet**. See
> [docs/cross-platform.md](docs/cross-platform.md).

---

## Keychain access on macOS

Claude Code stores its OAuth token as the login-keychain item
`Claude Code-credentials`. ClaudeUsage tries three sources in order:
`~/.claude/.credentials.json` (usually absent), then
`/usr/bin/security find-generic-password`, then the native Security framework.
Settings shows which one won ("Credential: security CLI").

The `security` CLI comes before the native API on purpose. The item's ACL
admits `/usr/bin/security` without asking, while a direct read by an unsigned
app shows the **"ClaudeUsage wants to use your confidential information"**
dialog, and "Always Allow" does not stick across rebuilds or token refreshes.
Measured: ~32 ms with no prompt via the CLI, against a prompt on every rebuild
via the native API. See [docs/auth.md](docs/auth.md).

If you do see the dialog, the CLI read failed and the app fell back to the
native API. Clicking "Always Allow" lasts until the next rebuild or token
refresh. It is a local-machine authorization only and changes nothing about the
app's Terms-of-Service standing (see the ToS notice above).

---

## Data flow

```
Claude Code (claude CLI) login
        │   macOS: keychain item "Claude Code-credentials" (via /usr/bin/security)
        │   Linux / Windows: ~/.claude/.credentials.json
        ▼
credential, cached in memory between polls
        │
        ▼
usage poller thread (60 s)  →  POST api.anthropic.com/v1/messages (1 token)
        │                        read anthropic-ratelimit-unified-5h/7d-* headers
        │                        (429s carry them too)
        ▼
AppState  →  tray icon + popover (Tauri event)  →  history.csv on change
        ▲
status poller thread (5 min)  →  status.claude.com, independent of usage
```

---

## Why no iOS app or widget

An iOS widget would need the Mac's usage values synced to the phone, since
App Groups are per-device. The only viable sync (iCloud key-value store)
**requires a paid Apple Developer account** — a free Personal Team cannot use
the iCloud capability (Apple's tooling rejects it outright). With the
free-Apple-ID constraint this project is built on, there's no path to a working
iPhone widget, so the iOS app and widget were removed. The desktop tray app is
the deliverable.

---

## Known gaps

- **App icons** are placeholder slots.
- **Durability:** built on an undocumented/unsanctioned path (see the ToS notice).

---

## Sources

- Headers technique + keychain token reuse: established Claude usage monitors
  (e.g. github.com/CrocSwap/claude-meter, github.com/rjwalters/claude-monitor)
- Anthropic OAuth/credential policy: anthropic.com/legal/consumer-terms,
  code.claude.com/docs/en/authentication
- Liquid Glass API: developer.apple.com (WWDC 2025)
