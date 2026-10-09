# Claudometer privacy and system-effects contract

This document describes the behavior of the Claudometer `0.9.1` source tree. It
is an inventory of data access and system effects, not a general legal privacy
policy. The inventory was verified against the request, storage, registry,
process, notification, update, and power-management call sites in `src/` and the
optional Caps Lock helper in `extras/caps-led.ps1`.

## Summary

Claudometer reads existing Claude Code and Codex credentials to request usage
limits. It does not collect telemetry or analytics. It does not log provider
responses, upload crash reports, or send data to a Claudometer-operated server.

Raw usage responses and update metadata are kept in memory only. Claudometer
stores preferences, a random installation salt, salted account digests, alert
deduplication receipts, and bounded normalized usage snapshots locally.
Snapshots include selected source, optional plan, observation time, and a
provider retry deadline. It never stores provider access tokens in its files.

Flyout pace projections use the existing quota percentage, reset, window length,
and local clock in memory. The pace-color preference is stored in settings;
pace adds no history file, endpoint, or polling request.

Quota-display (`used`/`left`) and reset-format (`clock`/`countdown`) preferences
are stored in the existing settings file. Clicking a row value/reset or using
its Settings control changes the same global preference. Countdown formatting
uses the local clock in memory and repaints on the existing 30-second visible
flyout timer. Demo actions change these choices in memory only and never save.

Freshness tokens and the next-update footer use the existing in-memory reducer,
observation timestamps and retry deadlines. Clicking the footer or pressing
Enter/Space requests the existing manual refresh for all providers. While any
shown provider is cooling down, the footer waits until the latest deadline and
does not fetch. The header refresh keeps the existing per-provider gates.
The visible-only 30-second timer updates age and minute-granular copy; no new
endpoint, persisted field, timer, credential operation or child process is added.
Demo refresh actions remain side-effect free.

Windows, the selected provider, GitHub, the default browser, or the Claude Code
CLI may independently keep their own network, notification, process, or crash
records. Those records are governed by those components rather than by
Claudometer.

## Network activity

All direct requests are HTTPS GET requests. As with any network connection, the
destination can observe connection metadata such as the source IP address and
request time.

| Destination | Trigger | Data transmitted | Frequency | User control |
| --- | --- | --- | --- | --- |
| `https://api.anthropic.com/api/oauth/usage` | Startup, an eligible automatic refresh, or an eligible manual refresh | Claude OAuth access token in `Authorization: Bearer`, the `anthropic-beta: oauth-2025-04-20` header, and `claudometer/<version>` user agent; no request body | The configured refresh interval, from 30 seconds to 5 minutes (default 1 minute). Manual refresh uses the same debounce and rate-limit cooldown. | The interval is configurable. There is currently no Claude-provider off switch; exiting Claudometer or blocking the destination stops requests. |
| `https://api.anthropic.com/api/oauth/profile` | After a successful Claude usage request when the in-memory plan cache is missing or expired | The same Claude bearer token, beta header, and user agent; no request body | On the first successful usage fetch for the active account, then no more than hourly while its entry remains cached. An account switch can cause another request. | No separate control; it follows Claude refresh activity. |
| `https://chatgpt.com/backend-api/wham/usage` | Startup, an eligible automatic refresh, or an eligible manual refresh when the Codex section is enabled and a ChatGPT-login Codex credential is available | Codex OAuth access token in `Authorization: Bearer`, the Codex account ID in `chatgpt-account-id`, and `claudometer/<version>` user agent; no request body | The configured refresh interval, subject to the same debounce and rate-limit cooldown as Claude | The **Codex section** setting enables or disables these requests. It is enabled by default. |
| `https://api.github.com/repos/Dvaderfun/claudometer/releases/latest` and fixed manifest/signature assets under the matching GitHub release tag | Normal startup and later polling ticks, only when automatic update checks are enabled | `claudometer/<version>` user agent and, for the API call, the GitHub JSON accept header; no credential and no request body | Once at startup, then at most once every 24 hours per running process; one manifest and signature document per check, plus a rollback authorization/signature only for an advertised downgrade | The **Automatically check for updates** setting controls these requests. It defaults off for a genuinely new install; an existing settings document that predates the setting migrates to enabled. The exact manifest bytes must pass the embedded Ed25519 trust root and local policy before an update is offered. |
| Fixed GitHub release asset URLs under `https://github.com/Dvaderfun/claudometer/releases/download/<tag>/` | The user clicks **Install** for an authenticated available portable update | `claudometer/<version>` user agent; no credential and no request body | One required SHA-256 file and one exact-size architecture-specific executable download per install attempt | The download requires an explicit **Install** click. The checksum must match the signed manifest hash. The executable is verified in bounded memory before a candidate file is created. Initial URLs and every redirect must use HTTPS and an exact allowlisted GitHub release host. Managed or ambiguous installs instead show an explicit release-page action. |

Updater requests disable automatic redirects and validate each hop before
following it. Release metadata stays on `api.github.com`; release downloads are
restricted to `github.com` and GitHub's documented
`release-assets.githubusercontent.com` host. Provider requests retain their
existing HTTP-client redirect behavior.

Claudometer can also hand a URL or a network-capable operation to another
program:

| Owner of subsequent network activity | Trigger | Behavior and control |
| --- | --- | --- |
| Default browser | The user opens the About link, asks for Claude Code setup help, or explicitly clicks a managed/ambiguous/failed update's **Release** action | Windows opens `https://github.com/Dvaderfun/claudometer`, `https://docs.anthropic.com/en/docs/claude-code/getting-started`, or a fixed repository release-page URL. Update failure never opens the browser automatically. The browser, not Claudometer, owns any resulting requests, cookies, and history. |
| Claude Code CLI and default browser | The user explicitly chooses Connect/Reconnect | Claudometer launches `claude auth login`. Claude Code owns the authentication requests, browser flow, callback-code handling, refresh-token rotation, and credential writes. Claudometer creates no loopback listener. Clicking the account card again cancels the launched process tree. |

No other direct network request site exists in this source tree. In particular,
there is no telemetry, product analytics, advertising, response logging, remote
diagnostics, or crash upload.

### CI network allowlist

Fixed network and browser destinations are centralized in `src/network.rs`.
The source gate fails if a runtime URL appears elsewhere, if a destination
constant is not named and documented here, or if the direct HTTP request sites
stop matching this list. Adding a URL or request site therefore requires a
reviewable update to this contract.

| Constant | Destination or delegated browser target |
| --- | --- |
| `ANTHROPIC_USAGE_URL` | `https://api.anthropic.com/api/oauth/usage` |
| `ANTHROPIC_PROFILE_URL` | `https://api.anthropic.com/api/oauth/profile` |
| `CODEX_USAGE_URL` | `https://chatgpt.com/backend-api/wham/usage` |
| `CODEX_BACKEND_URL` | `https://chatgpt.com/backend-api/` — fixed app-server backend; delegated GETs include `wham/usage`, `wham/accounts/check`, and the historical business/education/enterprise `wham/config/bundle` path; the adapter supplies local plan hint `unknown` to suppress that cloud-policy path. Usage may include the read-only reset-credit summary query. |
| `GITHUB_LATEST_RELEASE_URL` | `https://api.github.com/repos/Dvaderfun/claudometer/releases/latest` |
| `GITHUB_REPOSITORY_URL` | `https://github.com/Dvaderfun/claudometer` |
| `GITHUB_API_HOSTS` | Update metadata is restricted to `api.github.com` |
| `GITHUB_RELEASE_HOSTS` | Fixed release downloads and redirects are restricted to `github.com` and GitHub's documented `release-assets.githubusercontent.com` host |
| `CLAUDE_CODE_GETTING_STARTED_URL` | `https://docs.anthropic.com/en/docs/claude-code/getting-started` |

<!-- PRIVACY_REQUEST src/api.rs|ANTHROPIC_USAGE_URL -->
<!-- PRIVACY_REQUEST src/api.rs|ANTHROPIC_PROFILE_URL -->
<!-- PRIVACY_REQUEST src/codex.rs|CODEX_USAGE_URL -->
<!-- PRIVACY_REQUEST src/updater.rs|&current -->

### Isolated Codex app-server (owner-approved ADR 0007)

The `Codex app-server` Settings switch prefers the documented JSON-RPC source
only when the native executable matches an audited hash and system Codex
config/requirements files are absent. Unsupported/missing installations select
Compatibility before a request. App-server errors never cause a compatibility
HTTP request in the same cycle. Diagnostics/UIA report the selected source and
fixed fallback reason. This preference and `reset_credits_available` are
additive fields in local schema 1; reset credits are display only.

The worker reads the existing access token/account ID and passes only those
values through private inherited stdin. The child has ephemeral external auth,
no refresh token/API key, a fresh `CODEX_HOME`, a cleared environment, disabled
analytics/OTEL/plugins/remote control/runtime metrics, and a static model
catalog. Every server request, including token refresh, ends the cycle without
returning tokens. Managed login/logout, turns, reset-credit consumption, and
other mutations are never requested. Generic upstream JSON-RPC error messages
are discarded; they are not classified by text or copied to diagnostics.

The child performs read-only usage and account discovery against the fixed
ChatGPT backend. It supplies a local `unknown` plan hint to suppress enterprise
cloud-policy loading; the displayed plan comes from the quota response.
Requests transmit
the existing access token, account ID, and Codex client metadata. Only one
short-lived child runs per eligible Codex poll (30 s–5 min). Disabling this
switch selects the direct compatibility request; disabling Show Codex stops
both. No daemon, resident helper, Node process, or loopback server is added.

Each child owns `%TEMP%\claudometer-codex-run-<random>\`, containing an
exclusive `owner.lock`, synthetic static model catalog, Codex startup SQLite
databases/WALs, built-in skills/helper files, installation ID, and startup
files. The local unknown-plan hint prevents the historical cloud
bundle/raw-account-ID cache in both audited versions. Credential files are never
created there. Stdout is bounded to 1 MiB per frame; stderr is discarded and
Codex logging is disabled. A kill-on-close Job Object contains the child and
descendants from creation. Completion/failure kills the tree and removes the
scratch profile. A crash may leave scratch files; the next normal Codex poll
removes only unlocked owned profiles without following reparse points. Cleanup
failure reports a fixed local error. Provider-owned configuration is never
modified or deleted.

`--measure-codex-source` is an explicit maintainer-only live measurement,
authorized for CODEX-02. It reads local identity/state without migration,
performs one isolated quota read, and prints only source, elapsed milliseconds,
row count, and fixed error code. It exits before tray/windows/alerts/updates;
automated tests never use it and never call a live provider.

## Local files

`%APPDATA%\Claudometer` is Claudometer's application-data directory. The
directory is created as needed. Claudometer does not currently provide a
delete-data command or remove all application data on exit.

| Path | Reads and writes | Retention and deletion behavior |
| --- | --- | --- |
| `%APPDATA%\Claudometer\settings.json` | Reads preferences and legacy migration data. Writes the refresh interval; Codex, pace-color, quota-display, reset-format, alert, automatic update-check, wake-lock, and lid-override settings; and legacy alert receipts. Unknown JSON fields are preserved. | Retained until manually deleted. A new installation may have no settings file until a setting or legacy receipt is first saved. |
| `%APPDATA%\Claudometer\state.json` | Created on first normal startup. Stores a random 32-byte installation salt, salted SHA-256 account digests, alert receipts/migration markers, and a versioned `provider_cache`: at most two providers with selected source, optional plan, observation time, up to 64 normalized quota rows each (stable ID/kind/class, label, percentage, severity, reset/duration), and the accepted 429 retry deadline. Cache writes follow accepted fetches; authentication/preparation invalidation clears the provider's cache. It stores no access token, raw provider account ID, or response body. | Retained until manually deleted. Snapshots older than eight days, future observations, and reset-expired limit values are ignored on restore. Provider/source/account identity must match a locally prepared credential identity; token-fingerprint accounts never persist. Cached values are never fresh and do not trigger alerts. A valid future retry deadline blocks requests after restart. Deleting primary and backup resets cache/salt/receipts; a surviving verified backup may be recovered. |
| `settings.json.bak`, `state.json.bak` | Verified previous versions maintained by the atomic JSON writer. | At most one normal backup per primary file; replaced by later successful writes. Retained until manually deleted. |
| `%APPDATA%\Claudometer\diagnostics.log`, `.log.1`, `.log.2` | Normal startup and operational failures/successes append a UTC timestamp and a fixed sanitized event code. Rendering/configuration/registry/update/provider/clipboard failures are visible locally. Unknown strings are replaced by `[redacted]`; credentials, identifiers, response bodies and personal paths are never accepted as log fields. | At most three files of 256 KiB each, oldest replaced on rotation; repeated identical failure codes are suppressed. Safe to delete; deletion loses support history only. Log write failure is reported in Diagnostics. Demo and support commands do not create or append logs. |
| `settings.json.corrupt.*`, `state.json.corrupt.*` | A malformed primary file is renamed to a timestamped preservation copy before backup recovery or safe defaults are used. | Preserved indefinitely for manual inspection or deletion. There is no automatic retention limit. |
| Sibling `*.tmp.*` and `*.restore.*` files | Temporary files used for write-through atomic commits and backup restoration. | Removed after normal success or handled failure. A process or machine crash can leave debris; there is no startup sweep in this version. |
| `%APPDATA%\Claudometer\power-override.v1.json` and `.bak`/`.corrupt.*` siblings | Crash-recovery journal containing the power-scheme GUID, original and applied AC/DC lid-close values, operation phase, restoration progress, start time, and app version. | The primary and backup are deleted after verified restoration. They are retained while an override is active or recovery is incomplete. Malformed preservation copies remain until manually deleted. |
| `%APPDATA%\Claudometer\icon.ico` | The embedded app icon is extracted for Windows toast registration and replaced if its byte length differs. | Retained until manually deleted. |
| `%APPDATA%\Claudometer\alert-test.txt` | Written only by the hidden `--test-alert` verification mode with `ok` or a toast error. | Overwritten by a later test; otherwise retained until manually deleted. |
| The portable running executable and adjacent `claudometer.new.exe` / `claudometer.old.exe` | An explicit portable update authenticates the release policy, verifies the downloaded bytes against its signed exact size/hash in memory, then writes `.new`, verifies PE version/hash again, revalidates the install channel, renames the running executable to `.old`, renames `.new` into place, and relaunches it. Managed and ambiguous installs never enter this path. | Invalid manifests, policies, checksums, sizes, hashes, or PE magic create no candidate file. `.new` is removed after handled post-write validation failures; a process or machine crash can leave it. `.old` is normally deleted by the replacement process after handover and is also removed before a later swap attempt. Failures or crashes can leave update debris. |
| `%LOCALAPPDATA%\Programs\Claudometer\claudometer.install-channel` | A managed installer writes the exact UTF-8 value `managed`; Claudometer reads it when classifying the install channel. Claudometer does not create or modify it. | Retained for the managed install lifetime and removed by its installer/uninstaller. A missing, unreadable, or malformed marker blocks self-update at the managed path. |

### Provider and helper files

These files are owned by other tools. Claudometer does not write or delete
them.

| Path | Data read | When |
| --- | --- | --- |
| `%CLAUDE_CONFIG_DIR%\.credentials.json`, or `%USERPROFILE%\.claude\.credentials.json` | Claude OAuth access token, optional expiry hint, and optional `claudeAiOauth.scopes` | Account status checks and each attempted Claude refresh; an existing scopes array without `user:profile` blocks request preparation and asks for a normal Claude sign-in. A changing file is also used to detect completion of explicit Claude Code sign-in. |
| `%CLAUDE_CONFIG_DIR%\.claude.json`, or `%USERPROFILE%\.claude.json` when no custom directory is set | Cached Claude account/organization identifier and organization type | Account status and Claude request preparation |
| `%CODEX_HOME%\auth.json`, or `%USERPROFILE%\.codex\auth.json` | Codex OAuth access token and account ID; the token's JWT expiry claim is decoded locally | Each attempted Codex refresh while the Codex section is enabled |
| `%CLAUDE_CONFIG_DIR%\hooks\caps-led.ps1` and `%CLAUDE_CONFIG_DIR%\settings.json`, or their `%USERPROFILE%\.claude` equivalents | File presence and Claude hook configuration referencing `caps-led.ps1` | Settings rendering and Caps helper toggles, only when the optional helper appears to be installed |
| `%CLAUDE_CONFIG_DIR%\hooks\caps-led.disabled`, or its default-directory equivalent | Marker presence | Settings rendering; disabling the helper writes the marker, and enabling it deletes the marker |
| `%TEMP%\claude-caps-working.flag` | Optional helper heartbeat/working flag | Managed by `extras/caps-led.ps1`, not by the Claudometer executable; removed on done/end and recreated while flashing |

Provider response bodies, profile-plan cache entries, update metadata, and
bearer tokens remain memory-only. Only normalized snapshot fields listed above
are persisted; labels/kinds/plan strings are bounded to 512 UTF-8 bytes,
IDs to 128 bytes, and timestamps/durations are validated. Account/source
mismatch or invalid optional cache data disables restoration, preserving the
existing installation salt and alert-receipt envelope.
Windows notification history may retain the visible text of shown alerts under
the user's Windows notification settings.

## Credentials

Bearer tokens are held in worker memory and sent only to their corresponding
provider, with the ADR 0007 private-stdio handoff to the isolated Codex child:

Provider failures use fixed categories/codes and local recovery text. HTTP
401/403 clears account-bound displayed/cache data; 429 keeps same-account
last-good values and persists its bounded retry deadline. Error response
bodies/status text and transport error strings are discarded and never shown
or logged. Native flyout tooltips and UIA HelpText expose only generated
recovery details. Optional Claude scopes are checked locally without login,
token exchange, credential writes, or network requests.

- A Claude token is sent only to `api.anthropic.com` for Claude usage and
  profile requests.
- A Codex token is sent only to `chatgpt.com` for Codex usage requests. Its
  associated account ID is sent in the same request.

Claudometer does not read provider refresh tokens into its data model, exchange
or rotate tokens, write credential files, include tokens in local diagnostics,
or send one provider's token to the other provider. The `SecretString` buffers
owned by Claudometer are zeroed before their allocations are released. When no
stable account identifier is available, a token can be used locally to derive a
memory-only account digest; the token itself is not persisted.

## Registry and Windows integration

Settings → Diagnostics shows a local operational snapshot: version,
architecture, Windows build, install channel, provider detection/sign-in
booleans, selected source/fallback, attempt/success/observation timestamps,
freshness/age, retry deadline, stable error code, settings, and update/power
recovery status. It reads existing in-process state, the Windows version,
existing channel metadata, and presence of the adjacent update journal.
It does not probe provider endpoints or start a CLI. Local diagnostics logs
are described in the files table; there is no diagnostic upload. Provider account identifiers, emails,
tokens, plan/limit labels, response bodies, and home paths are excluded.

Explicit **Copy diagnostics** writes that snapshot as Unicode text to the
Windows clipboard. Clipboard retention follows Windows/user clipboard
settings (including any enabled clipboard history/sync); Claudometer does
not clear the clipboard automatically. Demo mode tests the action without
writing the clipboard or reading user state.

`--version` prints the application version and exits before normal startup.
`--diagnose` prints the same sanitized snapshot to stdout. It reads settings,
runtime state, existing installation metadata, and local provider credential
identity without writing/migrating/recovering any file, creating tray/windows,
logging, launching a CLI, or contacting provider/update endpoints. Corrupt or
missing state is reported locally; recovery remains normal startup's job.
Terminal redirection/retention is controlled by the invoking user.

| Location or API | Access | Trigger and retained effect |
| --- | --- | --- |
| `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme` | Read only | Read while choosing the Windows light/dark appearance; Claudometer does not change it. |
| `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Claudometer` | Read, create/update, or delete | Read to render the **Start with Windows** setting. Enabling stores the quoted current executable path; disabling deletes the value. The value remains until disabled or manually removed. |
| `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Claudometer_is1\InstallLocation` | Read only | Read before offering or applying an update when the executable is in the managed install root. It must agree with the adjacent managed-channel marker and executable directory; otherwise self-update is blocked. |
| `HKCU\Software\Classes\AppUserModelId\Claudometer` | Create/update `DisplayName` and `IconUri` | Written on every normal startup so an unpackaged executable can send Windows toasts. This happens even when usage alerts are disabled. The key is not deleted by Claudometer in this version. |
| Process AppUserModelID `Claudometer` | Set for the running process | Set on normal startup for toast routing; it lasts for the process lifetime. |
| Windows CNG SHA-256 | Read-only in-process hashing | Hashes a downloaded portable update before any executable rename. No child process or provider credential is involved. |

Claudometer creates a named single-instance mutex, window classes, tray icon,
and graphics resources. Those are process-scoped Windows objects, not durable
records.

## Child processes and URL launches

| Program or operation | Trigger | Purpose |
| --- | --- | --- |
| Native `claude.exe`/`claude.com`, or `cmd.exe` for a batch shim | Explicit Connect/Reconnect | Runs `claude auth login` in a visible console. Claude Code can open the browser and write its own credentials. |
| `taskkill.exe /T /F` | The user cancels Claude sign-in or the ten-minute login deadline expires | Terminates the launched Claude Code process tree; Claudometer also asks the direct child to exit and waits for it. |
| The replacement Claudometer executable with `--swap-wait` | Successful update file swap | Waits for the old single-instance mutex owner to exit, then starts normally and removes `.old`. |
| Hidden `powershell ... caps-led.ps1 end` | The user disables an installed Caps helper in Settings | Stops helper flashing and turns the LED off. The optional script can itself start a hidden PowerShell flasher when Claude Code invokes its hooks. |
| Windows `ShellExecute` with the `open` verb | Link/help/update actions described in the network section | Delegates the URL to the user's registered handler, normally the default browser. |

No shell or child process is used for normal provider polling. Short-lived
in-process worker threads read credentials and keep the prepared bearer token
local while the UI thread validates the opaque account/request identity.
Only an accepted request ticket permits HTTP execution. The message queue
contains opaque identities, sanitized outcomes, and reply handles, never
bearer tokens or credential contents. Alerts consume accepted successful
transitions on the UI thread; obsolete outcomes produce no side effects.

## Notifications

On normal startup Claudometer registers the `Claudometer` AppUserModelID and
local icon. When alerts are enabled, a freshly fetched limit at or above 75%
can produce one Windows toast per provider, account digest, limit, threshold,
and reset-window instance. A toast can contain the provider name, limit label,
percentage used, and reset time. Clicking it opens Claudometer's flyout.

Toasts use the Windows notification pipeline and respect per-app/user
notification settings and Focus Assist/Do Not Disturb. If WinRT toast delivery
fails, Claudometer can use a local tray balloon. If Windows reports that
notifications are disabled, Claudometer does not use the balloon fallback.
Alerts can be disabled in Claudometer Settings; registration files and registry
values remain.

## Power and Caps Lock effects

**Wake lock.** When enabled, Claudometer calls
`SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED |
ES_DISPLAY_REQUIRED)` to request that the system and display remain awake. The
preference is stored in `settings.json`; the request is re-applied at startup
and cleared on normal exit or Windows session shutdown. It does not rewrite a
power plan.

**Lid-close override.** When explicitly enabled, Claudometer reads the active
Windows power scheme, journals its current AC and DC lid-close actions, changes
both actions to **Do nothing**, activates that scheme, and verifies the result.
It restores the journaled values on disable, normal exit, or session shutdown.
On startup it first attempts recovery from an existing journal and then
re-applies the saved preference. While active, a polling tick or power broadcast
detecting a different active scheme restores the prior scheme's values and
applies a newly journaled override to the new scheme. Restoration does not
overwrite a value that another actor changed away from Claudometer's applied
value. Failed or interrupted recovery retains an actionable journal.

**Optional Caps Lock helper.** Claudometer does not install the helper or edit
Claude's hook configuration. If `caps-led.ps1` and a matching Claude hook are
already present, Settings can disable it by writing `caps-led.disabled` and
launching the script in `end` mode, or enable it by deleting that marker. The
script uses temporary DOS device aliases and
`IOCTL_KEYBOARD_SET_INDICATORS` to change the keyboard's Caps Lock LED without
changing the logical Caps Lock typing state. Claude Code hook invocations can
create/remove `%TEMP%\claude-caps-working.flag`, start a hidden PowerShell
flasher, and blink or set the LED. The helper performs no network request.

## Verification map

The principal implementation sources for this contract are:

- Provider requests and Claude credentials: [`src/api.rs`](src/api.rs)
- Codex requests and credentials: [`src/codex.rs`](src/codex.rs)
- Refresh/update triggers: [`src/main.rs`](src/main.rs); provider state and
  worker handoff: [`src/app.rs`](src/app.rs), [`src/poller.rs`](src/poller.rs)
- Update requests, files, processes, and links:
  [`src/updater.rs`](src/updater.rs)
- Settings, runtime state, and atomic retention:
  [`src/config.rs`](src/config.rs),
  [`src/runtime_state.rs`](src/runtime_state.rs), and
  [`src/store.rs`](src/store.rs)
- Registry, Caps marker, and helper launch: [`src/util.rs`](src/util.rs)
- Claude Code process ownership: [`src/auth.rs`](src/auth.rs)
- Toast registration and content: [`src/alerts.rs`](src/alerts.rs)
- Power changes and recovery journal: [`src/vibecode.rs`](src/vibecode.rs)
- Optional Caps Lock script: [`extras/caps-led.ps1`](extras/caps-led.ps1)

Any change that adds a destination, persisted field, registry value, child
process, notification payload, credential use, or system effect must update this
document in the same change.
