# Qt retirement inventory: hgs and Zerus Next

**Status:** Approved inventory, 2026-10-29; G1 topology resolved, 2026-10-29
**Source baseline:** `251ff068071604bc2478d740c687ff591b018c59` (`integrate/client-webui` at final UI-parity review)
**Epic:** `zerus-5wmh.66`; inventory task `zerus-5wmh.66.1`

## Goal and retirement rule

Remove the Qt GUI completely without losing a user-visible responsibility or leaving work that depends on a Qt process or file. Zerus Next owns interactive UI. `hgs` or one of its durable services owns every behavior and every piece of state that must survive closing the client window. Existing CLI commands, `HGS_*` variables, state/config paths and application/service identifiers remain compatible. The macOS switch (`zerus-5wmh.13`) is only the first platform step; Linux and Windows must reach the same retirement state.

This inventory covers every tracked file under `tray/src`, its associated Qt tests, the Qt-side installers and OS units, one-time migration of Qt-only data, and AUR/Homebrew/winget/Dev-feed packaging. At this baseline `tray/src` contains **294 files and 60,689 lines** (the original task said 284). “Merged” means present in this integration tree; a bead means existing work, not completed coverage. “Gap” has neither.

| Coverage marker | Meaning |
| --- | --- |
| merged client code | A concrete implementation exists under `client/` in the integration tree; it still needs retirement acceptance on each supported platform |
| bead ID | Existing scoped work under `zerus-5wmh`; this does not claim completion |
| gap | No merged implementation or existing bead was found; a replacement is required before removal |

No behavior is intentionally dropped unless the owner explicitly accepts that deviation after review.

## Phasing and platform order

The macOS switch (`zerus-5wmh.13`) keeps Qt as its rollback copy and does **not** wait for G1-G6 or the additional UI gaps in G7. The retirement program follows the switch, platform by platform:

1. **macOS first.** Remove Qt from the Mac only after G1-G3 and the macOS acceptance in G6, including the macOS durable-intent responsibility in G4. The rollback copy remains until that acceptance passes.
2. **Linux second.** Repeat migration, native integration and removal acceptance on the owner's Arch host before removing the Qt package and service payload.
3. **Windows last.** Apply the same gate to the portable zip, per-user installer, native shell integration and durable worker. Windows acceptance is currently **blocked** because the Windows PC is unreachable; this is a retirement blocker, not permission to skip Windows coverage.

G5 package removal is therefore per platform, not one global flag day. A platform's G6 acceptance must be green before its maintained installer can remove the Qt executable, plugins or rollback offer; already published assets remain immutable.

## Inventory

### Accounts, identity and usage

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Shared account identity grouping across machine-local profiles; provider launch policy by machine | `tray/src/AccountCatalog.h:13-91` | Zerus Next account projection: merged Accounts screen from `zerus-5wmh.8`; guarded write completion in `zerus-5wmh.25` |
| Agent/account rows and immediate account filters with stable bound snapshots and counts | `tray/src/AccountChoice.h:1-57`; `tray/src/AccountFilter.cpp:12-150`; `tray/src/AccountFilter.h:8-39` | Zerus Next: merged account choices/filtering from phase 3a (`zerus-5wmh.8`); archive-only filter choices remain `zerus-5wmh.45` |
| Quota windows, reset/pace forecasts, warning tones, accessible meter/panel rendering and 30-second countdown updates | `tray/src/AccountUsage.h:21-385` | Zerus Next: merged usage presentation from phase 3a (`zerus-5wmh.8`); settings preference under `zerus-5wmh.24` |
| Per-host/profile/home usage snapshots, revision guards, five-minute refresh, two concurrent reads and offline preservation | `tray/src/AccountUsageStore.cpp:3-64`; `tray/src/AccountUsageStore.h:12-40` | Interactive reads belong to Zerus Next: merged account host (`zerus-5wmh.8`). Any future quota-driven background action must use `hgs`, not this GUI cache |
| Accounts screen: catalog loading/warming, 30-second visible refresh, account details, sign-in/install/OpenCode start, add/copy/rename/remove/restore/default, permissions and model defaults | `tray/src/AccountsPage.cpp:40-1111`; `tray/src/AccountsPage.h:14-91` | Zerus Next UI: merged read screens (`zerus-5wmh.8`) and merged/follow-up writes (`zerus-5wmh.25`, phase 4 account/model work in `zerus-5wmh.9`). All credential/native effects remain guarded `hgs` operations; retirement must not move them into webview-only state |

### Activity, messages and media presentation

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Safe Markdown/HTML journal rendering with no arbitrary local/network resource loads; accessible selection, copy, expansion and external/file links | `tray/src/ActivityView.cpp:73-149,238-278,333-455,1237-1310,1543-1765`; `tray/src/ActivityView.h` | Zerus Next: merged Activity (`zerus-5wmh.2` family); file-reference links/dialog are `zerus-5wmh.60` |
| Native/provider/local event reconciliation, attachment-receipt identity, deduplication, clear boundaries, chronological grouping and recorded-context fallback | `tray/src/ActivityView.cpp:365-435,487-721,816-1175` | Zerus Next transcript projection: merged Activity and provider-specific phase 4 work (`zerus-5wmh.9`); exact parity must be retained as retirement acceptance, not replaced with raw terminal output |
| Voice/video cards, delivery states, transcript expansion, audio seek/play/pause and video dialog; playback stops when hidden or changing sessions | `tray/src/ActivityView.cpp:760-783,977-1028,1312-1336,1674-1867,2154-2187` | Zerus Next plus its media implementation (`zerus-5wmh.9`); device/background capture belongs to the Rust media/service layer, not a widget lifetime |
| Queue banner and Send now; retry/delete/inspect of failed local messages; OpenCode queued removal; task-document editing and recorded-process links | `tray/src/ActivityView.cpp:918-1141,1281-1305,1738-1764` | Merged queue panel and Send now in `client/core/src/input_queue.rs`, `client/src-tauri/src/composer/queue.rs` and `client/src/ui/sessions/QueuePanel.tsx` (closed `zerus-5wmh.9.24.1`, `.9.24.2`); failed-row Retry/Delete/Check in `client/src/ui/sessions/local-actions.ts`; task-document UI remains `.10`; process identity/read remains `hgs` |
| Follow-latest/unseen/search-result navigation and visible-reply acknowledgement | `tray/src/ActivityView.cpp:458-484,1490-1736,1883-2134` | Zerus Next: merged Activity/read-mark behavior; offline unread correction is `zerus-5wmh.59` |
| Bounded background document/layout generation (at most two workers), stale-generation rejection, anchor-preserving replacement and shutdown drain | `tray/src/ActivityView.cpp:1178-1234,1883-2149` | Zerus Next bounded rendering/work scheduling: merged client implementation plus performance follow-ups `zerus-5wmh.36`; no detached work may outlive the client unless moved to `hgs` |
| Compaction progress animation, only while the live session is compacting | `tray/src/ActivityView.cpp:75-101,1270-1280,1883-1906` | Zerus Next motion/lifecycle: merged status presentation; minimized-motion correction is `zerus-5wmh.39` |

### Analytics, archives, attention and approvals

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Bounded private agent/speech reads with capability probes and local epoch-pinned configure/purge; closed analytics projections, aggregation and stale-source rejection | `tray/src/AnalyticsClient.cpp:5-74`; `tray/src/AnalyticsData.cpp:1-283`; `tray/src/AnalyticsClient.h`; `tray/src/AnalyticsData.h` | `hgs` already owns collection and mutations; Zerus Next has merged Analytics Settings (`zerus-5wmh.24`) for reads and guarded controls |
| Analytics screen: 90-day/filter/page UI, at most 32 routes read serially, current-vs-last-known handling, histogram details, retention/purge confirmation and private diagnostic export | `tray/src/AnalyticsSettings.cpp:26-310`; `tray/src/AnalyticsSettings.h:13-50` | Zerus Next: merged Analytics Settings (`zerus-5wmh.24`); preserve bounded routes/pages and fail-closed exports |
| Activity column width/full-width stored in Qt settings | `tray/src/ActivityWidth.h:8-44` | Merged Settings UI; both `workspace/activityWidth` and `workspace/activityFullWidth` are already seeded read-only by `client/core/src/settings.rs:103-110`. G3 retains only the versioned import record/quarantine and remaining ownership handoff |
| Archive range presets/custom local-day selection | `tray/src/ArchiveDatePicker.cpp:20-237`; `tray/src/ArchiveDatePicker.h:7-39` | Zerus Next: merged Archive filtering (`zerus-5wmh.32` family) |
| One bounded archive page plus next/previous cursors per host, 300 ms query debounce and cancellation on scope changes | `tray/src/ArchiveBrowser.cpp:12-91`; `tray/src/ArchiveBrowser.h:11-45` | `hgs` supplies bounded archive pages; Zerus Next has merged archive paging. Grouped archive presentation is `zerus-5wmh.40`, archive-only filter choices `zerus-5wmh.45` |
| Attention edge detection and durable identity tokens for main/child requests, replies and optional questions; focus/mute/suppression/retry rules | `tray/src/AttentionTracker.cpp:12-287`; `tray/src/AttentionTracker.h:13-35`; `tray/src/AttentionNotifier.h:8-24` | **hgs service** for detection/token persistence and notification intent with the window closed; Zerus Next only presents/activates it. macOS native posting is `zerus-5wmh.43`, but durable cross-platform detection and Linux/Windows posting/routing are **gaps** |
| Automatic-approval opt-in and paginated private journal UI | `tray/src/AutomaticApprovalSettings.cpp:13-134`; `tray/src/AutomaticApprovalSettings.h`; `tray/src/AutomaticApprovals.cpp:9-14` | Zerus Next: merged read-only Settings/journal (`zerus-5wmh.24`); enabling needs `zerus-5wmh.54`; migrate `automaticApprovals/enabled` and error without auto-enabling |
| Hidden-window 250 ms automatic-approval worker: bounded 512-row scan, two targets, durable reservation before answer, identity rechecks and no uncertain replay | `tray/src/AutomaticApprovals.cpp:28-128`; `tray/src/AutomaticApprovals.h:28-43` | **hgs** worker: `zerus-5wmh.54`; retirement blocker because the Qt process must not own this behavior |

### Files, terminal launching and shared UI utilities

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Content-addressed private attachment storage/read-back, safe filenames and bounded async I/O; shared preview/open fetches (three active, 256 path LRU) | `tray/src/AttachmentViewer.h:25-242` | Zerus Next cache/preview UI with `hgs` transfer operations: merged attachments (`zerus-5wmh.9`); Windows path/native validation `zerus-5wmh.52`; file references `zerus-5wmh.60` |
| Busy arc and one-second prompt-cache countdown/warnings | `tray/src/BusyIndicator.h:9-45`; `tray/src/CacheStatus.h:8-64` | Zerus Next: merged loading/cache presentation; visibility/focus motion follows `zerus-5wmh.39` |
| Bounded text measurement/elision/layout caches for card painting | `tray/src/CachedText.h:8-91` | Zerus Next rendering/performance implementation (`zerus-5wmh.36`); no persistent migration |
| Close-on-exec child descriptor policy and detached terminal ownership | `tray/src/ChildProcess.h:14-55`; `tray/src/CommandBackend.cpp:12-99`; `tray/src/CommandBackend.h:13-47` | Rust process/terminal launcher in Zerus Next: merged terminal launch (`zerus-5wmh.9`), native Windows proof `zerus-5wmh.52`; terminals must outlive client exit |
| Clipboard command mode: newline refusal, Klipper/wl-copy/native fallback, read-back where possible and advisory | `tray/src/ClipboardBackend.cpp:12-269`; `tray/src/ClipboardBackend.h:13-76` | Merged copy actions are `zerus-5wmh.19`; Rust `copy_text` exists, but the full Linux Klipper/wl-copy fallback chain/read-back advisory is **gap** in G4 |
| Clone-repository form and durable uncertain-operation receipt checks | `tray/src/CloneRepositoryDialog.cpp:18-82`; `tray/src/CloneRepositoryDialog.h:13-38`; `tray/src/RepositoryClone.cpp:8-102`; `tray/src/RepositoryClone.h:10-42` | Merged dialog/host in `client/core/src/repository.rs` (closed `zerus-5wmh.10.8`); `hgs` owns Git/request identity. Qt pending-operation import and the post-exit owner remain in G3/G2 |

### Durable drafts, caches, fleet state and presence

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Private per-key message/question/voice/task drafts with text/form state/cursors, immutable hashed attachments, owner decoding and safe rename | `tray/src/ComposerDraftStore.cpp:21-363`; `tray/src/ComposerDraftStore.h:12-72` | Merged editor and durable local store (`zerus-5wmh.9.11`), read-only Qt import (`.13.1`), archived composer (closed `.9.29`, `client/src-tauri/src/composer/send.rs`, with the archive key resolved before any live row), Saved drafts navigation `.9.34` and exit flush (closed `.9.30`). G3 adds the versioned import record/quarantine and G2 the post-exit durable owner |
| Single writer thread with coalesced snapshots, noncoalesced durability barriers, asynchronous index/load and shutdown flush | `tray/src/ComposerDraftStore.cpp:45-116,130-159,307-353` | The ordered Rust writer and exit flush are merged (`.9.11`; closed `.9.30` in `client/src-tauri/src/exit_flush.rs` and `client/src/store/exitFlush.ts`). Move the final ordering/durability contract to `hgs` with an explicit writer-handoff marker; the post-retirement durable owner is G2 |
| Adaptive composer chips/popovers, focus return and narrow-width priority, including Usage limit and Recovery chips | `tray/src/ComposerToolbar.cpp:16-365`; `tray/src/ComposerToolbar.h:13-118` | Merged composer presentation (`.9`); Usage limit and Recovery chips are `zerus-5wmh.72` |
| Windows `--console-run`: allocate a console, pass exact argv/handles, preserve failed output until Enter | `tray/src/ConsoleHost.cpp:10-168`; `tray/src/ConsoleHost.h:9-42` | Merged native host in `client/core/src/terminal_launch/console_host.rs` and `konsole_bus.rs`; mandatory Windows execution is `zerus-5wmh.52`, and packaging must preserve an entry point without `hgs-tray.exe` |
| Independent Activity/composer/Terminal content scaling and generated-markup scaling | `tray/src/ContentScale.h:7-60` | Zerus Next preference: merged Appearance Settings (`zerus-5wmh.24`); migrate `workspace/contentScale` once, read-only |
| Overview of machine health/metrics, active/attention/draft sessions and accounts; 15-second refresh and one-second elapsed updates | `tray/src/DashboardPage.cpp:16-506`; `tray/src/DashboardPage.h:15-56` | Zerus Next: merged Dashboard from phase 3a (`zerus-5wmh.8`); exact row/status contract and offline unread correction `zerus-5wmh.59` |
| Owner idle/lock sampling every 30 s, 5 s transition checks, two-minute active threshold and input fallback while window is hidden | `tray/src/DesktopPresence.cpp:8-82`; `tray/src/DesktopPresence.h:12-62` | **hgs** durable presence service, with platform sources and phone-alert routing. Existing client code does not supply a window-independent owner: **gap** |
| Native local folder picker and bounded remote directory browser (2,000 entries), safe create/read-back after uncertain mkdir | `tray/src/DirectoryDialog.cpp:24-326`; `tray/src/DirectoryDialog.h:3-48` | Zerus Next dialog; guarded `hgs dirs/mkdir` operations: phase 5 (`zerus-5wmh.10`) |
| Verified downloaded-file cache: source attribution, content hashes, safe publish/lookup, 60-second opening grace, bounded trim and preserved edited/unknown files | `tray/src/DownloadCache.cpp:51-305`; `tray/src/DownloadCache.h:8-54` | Rust core has cache operations for file references (`zerus-5wmh.60`); migrate `.zerus-files.json` read-only and preserve every file. A window-independent cleanup owner is **gap** if trimming is to continue after exit |
| Download folder/size policy, destructive-limit confirmation and 65-second usage/trim refresh | `tray/src/DownloadCacheSettings.cpp:15-86`; `tray/src/DownloadCacheSettings.h:7-23` | Zerus Next: merged Files Settings (`zerus-5wmh.24`); migrate `downloads/folder` and `downloads/cacheBytes`; post-exit cleanup is **gap** |
| Per-host watch-v1 consumer: NDJSON framing, 50 ms batches, catalog/activity/git subscriptions, fallback polling and bounded restart delays | `tray/src/EventStream.cpp:22-548`; `tray/src/EventStream.h:14-135` | Zerus Next has merged watch-v1 Rust core. A shared **hgs** event fanout for background workers is **gap**; a second GUI-independent worker must not depend on `EventStreams` |
| Safe external-file opening: executable/script classification, Mark of the Web/quarantine, reveal-in-file-manager and platform notices | `tray/src/ExternalFile.cpp:14-284`; `tray/src/ExternalFile.h:8-30` | Rust external-file implementation (`zerus-5wmh.9`); Windows native execution `zerus-5wmh.52`; preserve security behavior before packaging |
| Fleet snapshots and freshness, last-known sessions during offline transitions, local/shared read state, notification preference revision bridging and bounded pending-mark settlement | `tray/src/FleetState.cpp:18-532`; `tray/src/FleetState.h:66-96` | Zerus Next has merged fleet/read projections; shared reads remain `hgs`. One-time legacy read migration exists in client; offline unread correction is `zerus-5wmh.59` |
| Fresh Git status badges with verified-remote and conflict/overflow rules | `tray/src/GitStatusBadge.h:7-102` | Zerus Next: merged row badges from watch/poll data |
| Global dialog dismissal ordering without changing button roles/defaults | `tray/src/DialogButtons.h:10-49` | Zerus Next shared dialog behavior (`zerus-5wmh.11` accessibility review); no independent background owner |

### CLI transport and application configuration

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Fuzzy Go to popup for sessions/projects/accounts/machines/commands with keyboard navigation and close chords | `tray/src/GoToDialog.cpp:14-263`; `tray/src/GoToDialog.h:14-61` | Keyboard Settings and the `go.anywhere` chord are merged (`.24`), but no popup consumes that chord; the actual Go to dialog and command routing are **gap** in G7 |
| Exact session/catalog JSON projection, listing digests and one-in-flight local/peer reads | `tray/src/HgsClient.cpp:43-455,580-947`; `tray/src/HgsClient.h` | Rust watch/list projection in Zerus Next is merged; additive launch binding fields are `zerus-5wmh.55`, background count `zerus-5wmh.65`, archived processes `zerus-5wmh.63` |
| Bounded CLI reads/writes for accounts, machines, swarm, tasks, search, process inspection, presence, mobile and approval journal | `tray/src/HgsClient.cpp:608-783,889-947,1018-1047` | Keep the `hgs` commands/receipts; Zerus Next calls through its Rust hosts (`zerus-5wmh.8`, `.24`, `.25`, `.54`). No Qt transport is needed after retirement |
| Verified download/cache/attachment transfers and deferred cache trimming | `tray/src/HgsClient.cpp:949-1129` | `hgs` owns remote transfer; Zerus Next owns interactive presentation/cache (`zerus-5wmh.9`, `.60`); durable cleanup owner is **gap** |
| Guarded session lifecycle/actions, native UI, send/answer/send-now/effort/settings/notifications/read and directory/repository mutations | `tray/src/HgsClient.cpp:1132-1344,1873-2546` | `hgs` owns effects and identities; Zerus Next guarded actions/composer (`zerus-5wmh.9`, `.10`, `.24`) |
| Project catalog writes and bounded worktree catalogs/reviews/inspections with generation guards and 30-second snapshots | `tray/src/HgsClient.cpp:1346-1871` | `hgs` owns catalogs/mutations; Zerus Next Projects/Worktrees (`zerus-5wmh.10`, `.24`, `.25`, `.31`). Migrate `workspace/worktreeSnapshots` as a cache only, never ownership evidence |
| `tray.conf` path and tolerant atomic key/value updates; platform CLI/open-mode options, hgs selection, Windows staged CLI fallback, polling floors and restart argv | `tray/src/AppConfig.cpp:25-697`; `tray/src/AppConfig.h:18-123` | `hgs`/Zerus Next shared startup configuration: preserve selected CLI and platform modes; one-time read-only `tray.conf` import is **gap**. Normal polling must become an `hgs` subscription/service rather than a tray-only timer |
| Fuzzy navigation and global window-level UI helpers | `tray/src/GoToDialog.h:14-61`; `tray/src/DialogButtons.h:10-49` | Zerus Next UI; no `hgs` background owner |

### Native window, tray and platform integration

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Brand/count tray icon rendering, stable appearance cache and provider/machine badges | `tray/src/IconFactory.cpp:20-186`; `tray/src/IconFactory.h:12-37`; `tray/src/IdentityBadge.h:8-124`; `tray/src/IdentityHash.h:8-26` | Zerus Next native tray/dock icon (`zerus-5wmh.44` on macOS); Linux/Windows native equivalent is **gap** |
| Off-GUI JSON parsing/preparation and fatal GUI-thread contract checks | `tray/src/JsonParse.cpp:7-94`; `tray/src/JsonParse.h:9-53` | Rust bounded background parsing in Zerus Next is merged; retain test/failure semantics rather than the Qt checker itself |
| One-shot KWin script to inspect/change keep-above for exactly the matching PID/application/window and self-unload | `tray/src/KWinWindowLayer.cpp:15-154`; `tray/src/KWinWindowLayer.h:10-38` | Zerus Next Linux window integration: `zerus-5wmh.24.2` |
| Shortcut editor with conflict reassignment/reset and Terminal-safe markers | `tray/src/KeyboardSettings.cpp:12-209`; `tray/src/KeyboardSettings.h:11-25` | Zerus Next: merged Keyboard Settings (`zerus-5wmh.24`); customized chord migration is required |
| Linux Konsole launch/tab selection, security-sensitive D-Bus probe, exact argv, `/proc` tty matching and activation fallback | `tray/src/KonsoleBackend.cpp:17-431`; `tray/src/KonsoleBackend.h:15-58` | Zerus Next Linux terminal launcher: merged Konsole support (`zerus-5wmh.9`); verify non-window creation/terminal lifetime and tab fallback |
| Unity LauncherEntry count and shell-restart republish | `tray/src/LinuxApplicationBadge.cpp:8-52`; `tray/src/LinuxApplicationBadge.h:13-23` | **hgs** computes the durable attention count; Zerus Next publishes it on Linux: **gap** |
| Freedesktop notifications with persistent numeric-ID/token mapping, owner reset, activation token and withdrawal | `tray/src/LinuxAttentionNotifier.cpp:16-90`; `tray/src/LinuxAttentionNotifier.h:13-24` | **hgs** owns durable intent/token state; Linux native poster/router is **gap**. Migrate `attention/linuxOwner` and `attention/linuxTokens` read-only without re-posting old alerts |
| Linux idle/lock sources: X11 idle, Freedesktop/Mutter calls, KIdleTime thresholds and stale-source refusal | `tray/src/LinuxPresenceSource.cpp:12-136`; `tray/src/LinuxPresenceSource.h:13-47` | **hgs** presence service: **gap**, shared with the `DesktopPresence` row |
| Plain-file vs shortcut-target size/read rules | `tray/src/LocalFile.h:7-17` | Rust file/download implementation (`zerus-5wmh.9`, `.52`) |
| Windows session identity and confirmation before launching into a host in another session | `tray/src/LocalSessionHost.cpp:5-40`; `tray/src/LocalSessionHost.h:12-52`; `tray/src/LocalLaunchWarning.h:10-32` | Zerus Next native launch confirmation; `hgs session-host` owns staging. Windows proof is `zerus-5wmh.52` |
| macOS accessory/regular visibility, Dock reopen/minimize restoration and native application icon | `tray/src/MacAppVisibility.mm:8-89`; `tray/src/MacAppVisibility.h:8-20` | Zerus Next macOS lifecycle (`zerus-5wmh.44`) |
| macOS Dock badge | `tray/src/MacApplicationBadge.mm:5-20` | **hgs** supplies durable count; Zerus Next macOS badge in the menu-bar/Dock work (`zerus-5wmh.44`) |
| macOS native notification authorization, token posting/withdrawal, foreground presentation and click activation | `tray/src/MacAttentionNotifier.mm:7-122` | `zerus-5wmh.43` for native client posting; **hgs** must originate persistent intent/tokens with the window closed (**gap**) |
| Terminal.app AppleScript launch and notification escaping | `tray/src/MacBackend.cpp:11-68`; `tray/src/MacBackend.h:13-50` | Zerus Next native terminal launcher (`zerus-5wmh.9`); advisory/UX parity review |
| macOS input-idle/lock sampling without extra permissions | `tray/src/MacPresenceSource.cpp:6-36` | **hgs** presence service: **gap** |
| macOS sleep/wake notices acknowledged without vetoing sleep | `tray/src/MacSystemPower.cpp:9-67`; `tray/src/MacSystemPower.h:8-22` | Zerus Next native power integration feeding its watchdog (`zerus-5wmh.12.3`) |
| Native menu popup while preserving Qt/AppKit delegates | `tray/src/MacTrayMenu.mm:5-20`; `tray/src/MacTrayMenu.h:4-7` | Zerus Next native menu-bar integration (`zerus-5wmh.44`) |
| Cocoa application menu: About, Services, Hide, Hide Others, Show All and Quit | Qt platform plugin (no `tray/src` file) | Tauri's default menu with Quit replaced by `quit_with_flush` (closed `zerus-5wmh.9.30`); Show All remains open under `zerus-5wmh.73` |
| Machine label palette/vivid preferences and filter chips/counts | `tray/src/MachineAppearance.h:9-77`; `tray/src/MachineFilter.cpp:12-108`; `tray/src/MachineFilter.h:10-42` | Zerus Next merged filters/appearance; one-time migration of `machines/colors/*` and `machines/vivid/*`; archive-only choices `zerus-5wmh.45` |
| POSIX/Windows path semantics for local/remote catalogs and guarded mutations | `tray/src/MachinePlatform.h:9-152` | Rust shared path policy (`zerus-5wmh.9`, `.61`, `.52`); no Qt-specific persistence |
| Machines screen: effective SSH settings, add/edit/check/install/remove/enable-local, label editor and phones panel | `tray/src/MachinesPage.cpp:25-455`; `tray/src/MachinesPage.h:14-66` | Merged read-only Machines screen (`zerus-5wmh.8`) and mobile settings (`.24.4-.24.8`); write buttons are disabled and remain open under `.25`; native host operations remain `hgs` |
| Main-thread heartbeat watchdog with uptime clocks, sleep/wake grace and crash-triggered service restart | `tray/src/MainThreadWatchdog.cpp:11-100`; `tray/src/MainThreadWatchdog.h:21-88` | Merged native implementation in `client/src-tauri/src/watchdog.rs` (`zerus-5wmh.12.3`, in progress); LaunchAgent/systemd/Windows restart integration retained in packaging |

### Markdown and composer details

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| GitHub Markdown blocks/spans, safe autolinks, code chips, markers, plain-text copying and height-for-width disclosures | `tray/src/MarkdownHtml.cpp:18-552`; `tray/src/MarkdownObjects.cpp:30-758`; `tray/src/MarkdownText.cpp:10-131`; `tray/src/MarkdownHtml.h`; `tray/src/MarkdownObjects.h`; `tray/src/MarkdownText.h` | Zerus Next Activity/Markdown components: merged phase 1/4 implementation; file links are `zerus-5wmh.60`, rendering performance `zerus-5wmh.36` |
| Native monospace selection and theme-consistent confirmation icons/buttons | `tray/src/MonospaceFont.h:6-18`; `tray/src/MessageBoxIcons.h:10-76` | Zerus Next shared skin/dialog behavior (`zerus-5wmh.11`); no background state |
| Resizable message editor and per-key draft restore, suggestion/IME/send/push shortcuts and delivery locks | `tray/src/MessageComposer.cpp:64-855`; `tray/src/MessageComposer.h:29-141` | Merged composer and native queue/push (`.9`, closed `.9.24.1`/`.9.24.2`), duplicate safeguard (`client/src/ui/sessions/Composer.tsx:286`) and edge/gap ResizeHandle with pointer/keyboard reset (`Composer.tsx:53-89`); archived composer `.9.29`, suggestions/IME `.70`, exit flush closed `.9.30` (`client/src-tauri/src/exit_flush.rs`, `client/src/store/exitFlush.ts`) |
| Attachment pick/paste/drop/reference, limits, thumbnails/open/remove, external-file safeguards and immutable draft copies | `tray/src/MessageComposer.cpp:857-1125` | Zerus Next attachments (`zerus-5wmh.9`), native Windows proof `zerus-5wmh.52`; file references `zerus-5wmh.60` |
| Saved drafts index/preview/restore/copy/Go to with replacement confirmation and uncertain-send warning | `tray/src/MessageComposer.cpp:1231-1388` | Merged Saved drafts includes Qt's store through Task 7 and previous-app recovery (`client/src-tauri/src/composer/saved.rs`, `SavedDrafts.tsx`); archived composer (closed `.9.29`, `client/src-tauri/src/composer/send.rs`, with the archive key resolved before any live row), exact Go to `.9.34`, lazy loading `.9.37`; Qt import already exists under `.13.1` |
| Session model/effort controls and OpenCode picker catalogs/favorites with bounded per-context choices and confirmed writes | `tray/src/MessageComposer.cpp:1390-1667`; `tray/src/OpenCodeModelPicker.cpp:18-364`; `tray/src/OpenCodeModels.cpp:8-163` | Model/effort controls and the OpenCode picker are `zerus-5wmh.69`; `hgs` applies exact revision/receipt |
| Main/child subagent composer, per-child draft identity and guarded answer/send routing | `tray/src/MessageComposer.cpp:64-855`; `tray/src/SessionsWindow.cpp:687-1316` | Subagent composer parity is `zerus-5wmh.71`; durable child drafts use the existing store/import, with post-exit ownership in G2 |
| Shared phone-delivery preference, stale-worker/outdated-peer notices and away choices | `tray/src/MobileDeliverySettings.h:13-134` | `hgs` swarm owns the preference and presence-driven behavior; Zerus Next has merged Mobile Settings (`zerus-5wmh.24`); window-independent presence is **gap** |

### Native workspace

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Optional isolated native-browser interface and fixed-path plugin loading without linking Chromium into the main GUI | `tray/src/NativeBrowser.h:7-34`; `tray/src/NativeBrowserPlugin.cpp:10-71`; `tray/src/NativeBrowserPlugin.h:7-22` | Replace with Tauri/browser integration (`zerus-5wmh.9`); remove Qt WebEngine/plugin package dependencies after parity |
| Provider action/capability rules, especially OpenCode binding-only forget and Terminal support | `tray/src/NativeSessionCapabilities.h:7-54` | Zerus Next: merged action-capability projection |
| DeepSeek native workspace per-origin isolated profiles, exact loopback URL validation, reload/renderer-failure states and Respond in Activity | `tray/src/NativeUiView.cpp:10-115`; `tray/src/NativeUiView.h:12-45` | Zerus Next: phase 4 Native UI (`zerus-5wmh.9`); credentials stay in `hgs` launch response, never persistent browser/Qt migration |

### Session creation, projects and process control

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| New session target/source/folder/repository/worktree forms, admission and launch plan restoration | `tray/src/NewSessionDialog.cpp:41-1313`; `tray/src/NewSessionDialog.h:13-176`; `tray/src/NewWorktreeDialog.h:7-76` | Zerus Next: `zerus-5wmh.10.10` view and `.10.10.1` creation host, with launch fields `zerus-5wmh.55`; creation and Git/worktree effects remain guarded `hgs` calls |
| Launch account/model defaults, remembered agent/account/open-terminal preferences and provider-specific picker/service state | `tray/src/NewSessionDialog.cpp:137-475,744-1313`; `tray/src/OpenCodeEffort.h:6-15` | Zerus Next: same phase 5 work. One-time migration of `workspace/launchAgent`, `workspace/launchAccounts` and `workspace/launchOpenTerminal` is **gap** |
| Grouped searchable OpenCode catalog/favorites UI and validated provider/model/effort projections | `tray/src/OpenCodeModelPicker.cpp:18-364`; `tray/src/OpenCodeModelPicker.h:18-67`; `tray/src/OpenCodeModels.cpp:8-163`; `tray/src/OpenCodeModels.h:10-42` | Model/effort and OpenCode picker parity is `zerus-5wmh.69`; preserve bounded lists and exact context |
| Optional-question badge with no status/attention change | `tray/src/OptionalQuestionBadge.h:8-48` | Zerus Next: merged optional-question presentation |
| Phone pairing QR/manual code, expiry, workspace/relay pinning, phone list and confirmed revocation | `tray/src/PhonePairing.cpp:24-434`; `tray/src/PhonePairing.h:13-109` | Zerus Next: `zerus-5wmh.24.4` and `.24.6`; `hgs` owns invitations/revocations and connector state |
| Shared short-lived process worker and preparation thread, bounded output, exact-once results, child reaping and owner-death cancellation | `tray/src/ProcessRunner.cpp:21-556`; `tray/src/ProcessRunner.h:19-109` | Rust command/process execution in Zerus Next; long-lived work must use `hgs`, never a replacement detached GUI worker |
| Process settings and active/background output polling; “Show running commands” in the session context menu | `tray/src/ProcessSettings.h:6-18`; `tray/src/ProcessView.cpp:23-226`; `tray/src/ProcessView.h:18-61`; `tray/src/SessionsWindow.cpp:5298` | Merged Processes UI/cadence (`.9.32`, `.62`) and `runningCommandsEntry` (`client/src/store/processes.ts:257-264`), gated on online plus Processes enabled and checked again on activation; all five `processes/*` settings are seeded by `client/core/src/settings.rs:121-151`; archived inventory `.63`, row background count `.65` |
| Targeted native/process-tree stop with exact run/conversation/generation and no automatic replay | `tray/src/ProcessView.cpp:1-226` | `hgs processes --stop` through Zerus Next (`zerus-5wmh.9`); native verification on Windows `zerus-5wmh.52` |
| Projects page, ordering/default/folder links, legacy project import, clone and swarm connection hints | `tray/src/ProjectsDialog.cpp:305-638`; `tray/src/ProjectsDialog.h:19-101` | Read screens/organization are merged (`.31`, `.48`), but writes are open under `.25` and project actions under `.18`; legacy `projects.local` and Qt `workspace/organization` migration stay read-only |
| Folder edit/open/new-session/move/copy, project color/vivid and splitter preferences | `tray/src/ProjectsDialog.cpp:1-638`; `tray/src/ProjectAppearance.h:7-40`; `tray/src/ProjectFilter.cpp:11-116`; `tray/src/ProjectFilter.h:10-40` | Merged Projects/filtering; `workspace/allSwarmProjects` and `workspace/projectFolderSplit` still need G3 readers. The client's `organization-layout.json` is local per device (`.40`, `.47`), matching Qt's local QSettings; only shared organization reconciles through the `hgs` replica, not a second sync format |
| POSIX PTY and Windows ConPTY attach processes: bounded I/O queues, resize, hangup and owned-client cleanup only | `tray/src/PtyProcess.cpp:27-226`; `tray/src/PtyProcess_win.cpp:20-406`; `tray/src/PtyProcess.h:14-62` | Rust terminal backend in Zerus Next (`zerus-5wmh.9`), with ConPTY execution `zerus-5wmh.52`; never terminate the agent/server when a view closes |
| Native Qt tray activation and clickable message routing | `tray/src/QtTrayIcon.cpp:11-73`; `tray/src/QtTrayIcon.h:10-32`; `tray/src/ApplicationBadge.h:7-13` | Zerus Next native tray/menu integration: macOS `zerus-5wmh.44`, Linux/Windows **gap** |

### Questions, recovery and shared read delivery

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Question/approval forms with exact identity, multi-page answers, optional queue navigation, native body refresh and accessible validation | `tray/src/QuestionCard.cpp:37-633,768-1009`; `tray/src/QuestionCard.h:34-131` | Merged `QuestionCard.tsx`, `client/src/store/questions.ts` and `client/src-tauri/src/composer/questions.rs` (closed `zerus-5wmh.9.23.1`); answer delivery remains guarded `hgs answer` |
| Durable question answer drafts, three-second visible review, broader-permission second confirmation and submit/answered/uncertain states | `tray/src/QuestionCard.cpp:271-479,481-766,917-942` | Question UI is merged (`.9.23.1`), with exit flush closed under `.9.30` (`client/src-tauri/src/exit_flush.rs`, `client/src/store/exitFlush.ts`). `question\n…` entries already parse through the `.13.1` shared-draft import; `hgs` still owns pending delivery after exit |
| Optional-question notification preference and answer-preview parsing off the GUI thread | `tray/src/QuestionNotificationSettings.h:5-9`; `tray/src/QuestionReply.h:8-97` | Zerus Next Settings: `notifications/optionalQuestions` migration; answer preview projection in Rust |
| Read-mark delivery queue with four writes, per-host fairness, confirmed/partial/failed backoff and settlement through snapshots | `tray/src/ReadMarkSync.cpp:9-91`; `tray/src/ReadMarkSync.h:10-42`; `tray/src/ReadMarks.h:8-85` | Merged client/core and src-tauri `readsync.rs`, including closed exit flush `.53`; **hgs** still owns post-exit delivery/retry in G1 and `attention/pendingReads` plus `attention/sharedReadMigrated` migration in G3 |
| Quiet tray icon updates deferred during native menu tracking | `tray/src/QuietTrayIcon.cpp:7-104`; `tray/src/QuietTrayIcon.h:20-85` | Zerus Next macOS menu integration (`zerus-5wmh.44`); no durable state |
| Recovery policy replication and editor with per-class delays, reset mode, revision conflict refusal and offline preservation | `tray/src/RecoverySettings.h:12-152`; `tray/src/RecoveryWidgets.h:16-160` | `hgs` already owns durable recovery/policy execution; Zerus Next has merged Recovery Settings (`zerus-5wmh.24`). Migrate `recovery/sharedPolicy` read-only without changing retry state |
| Legacy relay configuration/private receipt directory validation and safe atomic edits, connector implementation/status and restart safety | `tray/src/RelayConfiguration.h:18-221`; `tray/src/RelaySettings.h:18-253` | `hgs`/connector owns configuration, journal and service. Zerus Next legacy relay UI is `zerus-5wmh.24.8`; settings import is read-only and never copies tokens. Retirement must not touch the `zerus-mobile-connector` unit or `~/.local/share/zerus-mobile` before `zerus-nmc8`; moving a live gateway is owner-approved |
| Repository acquisition identity and uncertain-result recovery without another clone | `tray/src/RepositoryClone.cpp:8-102`; `tray/src/RepositoryClone.h:10-42` | Merged `client/core/src/repository.rs` (closed `zerus-5wmh.10.8`); `hgs` owns the durable receipt. Migrate `workspace/repositoryClone/*/pending` read-only and retain previous destinations |
| Account binding codec and historical evidence used by session filters | `tray/src/SessionAccounts.h:7-119`; `tray/src/AccountCatalog.h:13-91` | Zerus Next: merged account projection; never persist historical home evidence as account membership |
| Session row/card/strip rendering, status priority, elapsed/working animation, badges and accessibility | `tray/src/SessionCardDelegate.h:23-687`; `tray/src/SessionStatusBadge.h`; `tray/src/SessionElapsed.h`; `tray/src/OptionalQuestionBadge.h` | Merged session cards and working clocks; motion `.15` and minimized-motion `.39`, background count `.65` |
| POSIX/PowerShell command quoting for attach/folder/clipboard operations, including Unicode quote and trailing-backslash rules | `tray/src/SessionCommands.h:10-120`; `tray/src/WindowsCommandLine.h:10-52` | Rust terminal launcher (`zerus-5wmh.9`); PowerShell parity `zerus-5wmh.61`, native proof `.52` |
| Shared scope-choice checkboxes and flowing chip layouts | `tray/src/ScopeFilterWidgets.h:5-62` | Zerus Next shared filter UI; no durable owner |

### Session creation, organization and row projection

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Ordered durable creation journal in `QSettings.fileName()+".creations.json"`; at most 32 unfinished operations, 96 visible history/128 stored records and four concurrent creations | `tray/src/SessionCreationController.cpp:22-445`; `tray/src/SessionCreationController.h:8-72` | Merged client journal in `client/core/src/creation.rs` (closed `zerus-5wmh.10.9`), creation UI/host `.10.10`/`.10.10.1` and launch binding `.55`; `.creations.json` import remains G3 and the post-exit owner G2 |
| Exact launch/worktree/repository identities, operation cancellation before dispatch, ended-launch evidence, placement inference and manual recovery without repeating uncertain work | `tray/src/SessionCreationController.cpp:58-434` | Merged recovery in `client/core/src/creation.rs` (`.10.9`) plus ended-launch evidence `.55`; `hgs` still owns the post-exit state machine in G2, with no repeated uncertain work |
| Working-time clocks, context/token/usage HTML and goal elapsed/status/metrics projections | `tray/src/SessionElapsed.h:5-34`; `tray/src/SessionUsage.h:8-100`; `tray/src/SessionGoal.h:7-55` | Zerus Next read-only status/details presentation: merged client UI; no independent durable owner |
| Attachment/file drop routing to main, child or task composer and shell-path drops | `tray/src/SessionFileDrop.h:7-78` | Zerus Next drag/drop and composer/terminal routing: merged attachments/terminal work (`zerus-5wmh.9`); Windows validation `.52` |
| Parse and resolve `hgs-file` references relative to the exact session/child cwd | `tray/src/SessionFileReference.h:5-55` | Zerus Next plus `hgs` download/open operations: `zerus-5wmh.60` |
| Session list interaction: keyboard/selection/drag organization, project/child expansion, compact-row toggles, context menus, “Sort sessions by name” and motion synchronization | `tray/src/SessionList.cpp:15-645`; `tray/src/SessionList.h:10-111`; `tray/src/SessionsWindow.cpp:2601` | Merged session list/navigation and project-background sort action (`client/src/ui/sessions/organization-ui.ts`, `.31`/`.47`); compact/collapsed state `.24.3`; remaining organization writes stay under `.25` |
| Per-session mute, review-later and notification preferences with revision-aware guarded writes | `tray/src/SessionNotifications.h:7-97` | Merged Mute/Mark all read controls (`.16`); `hgs` owns durable preference revisions/effects. Migrate local display/read markers without unmuting or replaying alerts |
| Stable organization identities, project/session ordering, folders, collapsed/default projects, implicit Inbox and explicit placement | `tray/src/SessionOrganization.cpp:8-338`; `tray/src/SessionOrganization.h:7-73` | Zerus Next organization `.31`, project actions `.18`, guarded writes `.25`, migration/bootstrap `.48` and follow-ups `.47`/`.49`; `hgs` replica owns shared state |
| Legacy run tokens and `run_groups`, archive/run identity continuity and preservation of explicit placement across renames/restores | `tray/src/SessionOrganization.cpp:16-168,236-338` | Existing read-only seeds are `.40` (`run_groups`, at most 20,000 entries, oldest first) and `.47` (run tokens, at most 100,000 members), both in progress; G3 covers the remaining `workspace/organization` families and quarantine without lexical inference or unknown-field loss |
| Collapse/dock/peek/strip behavior and adaptive narrow-session presentation | `tray/src/SessionPanelDock.h:7-208` | Zerus Next dock/responsive UI: `.24.3`; migrate size/hover preferences with general settings |
| Display title, status, checkout/project context, action explanation, freshness and merged inspection projection | `tray/src/SessionPresentation.h:8-403`; `tray/src/SessionRows.h:8-98`; `tray/src/SessionStatusBadge.h:6-94`; `tray/src/SessionTag.h:5-19` | Zerus Next merged list/status projection; offline unread `.59` and background count `.65`; no durable duplicate status state |
| Bounded compact-row identities, at most 50,000 entries with observed identity cleanup | `tray/src/SessionRowPreferences.h:7-51` | Zerus Next `.24.3`; one-time migration of `workspace/collapsedSessionRows`, preserving bounded identity semantics |
| Bounded live/archive content search, per-host generation/cancellation, search-result and creation activation | `tray/src/SessionSearch.cpp:12-317`; `tray/src/SessionSearch.h:8-61` | Only the merged name/ID filter exists in `SessionList.tsx`; content search and archive search have no implementation or bead and are **gap** in G7. Archive paging `.40` and filters `.45` do not supply content search |
| Multi-select lifecycle, read/mute/review actions, per-machine Pause all/Resume all in background and exact preconditions/receipts | `tray/src/SessionSelection.cpp:10-400` | Merged batch actions (`.20`) and read/mute controls (`.16`), including per-machine counts and the OpenCode individual-controls notice; lifecycle/worktree behavior is `.10`/`.17`. `hgs` owns effects; preserve confirmation and never replay failed/uncertain operations |
| Termination/archive worktree-removal choices, recorded checkout ownership and cleanup-result handling | `tray/src/SessionSelection.cpp:127-400` | Zerus Next confirmation/review plus `hgs` background cleanup. Migrate `workspace/removeWorktreeAfterTermination`; unknown/replaced ownership stays manual and no ignored data is removed |
| Width-aware accessible tooltip HTML | `tray/src/SessionTooltip.h:7-52` | Zerus Next shared presentation; no persistent owner |

### Main workspace window and delivery orchestration

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Main rail/pages, Projects/Machines/Accounts/Settings navigation, responsive splitters, themes, saved geometry and “Hide launch status” | `tray/src/SessionsWindow.cpp:265-655,1500-1775,2034-2500,5831-5908`; `tray/src/SessionsWindow.cpp:505`; `tray/src/SessionsWindow.h:69-700` | Merged workspace navigation/Appearance (`.24`); migrate `sessions/geometry`, `workspace/splitter`, inspector/dock/theme/list preferences. “Hide launch status” has no corresponding client control and is **gap** in G7 |
| Session/context actions, identity/reference/folder commands, Copy connect/restore command, rename/fork/clear/start-fresh/pause/resume/archive/forget/terminate and per-machine bulk actions | `tray/src/SessionsWindow.cpp:635,3894,5306`; `tray/src/SessionsWindow.cpp:2844-3127,4638-4804,5222-5830` | Merged guarded actions over `hgs` receipts: batch `.20`, marks/mute `.16`, Start fresh/Restore/Forget/Clear archive `.17`, Clear/Compact `.9.18`, Fork `.10.7`, copy actions `.19` (including Copy connect/restore in `client/src/ui/sessions/actions-ui.ts:322-333`), lifecycle `.10` and rename `.9.35`; exact identities/confirmations remain retirement acceptance |
| All/attention/active/saved/archive filters, machine/project/account/folder scopes, creation rows, project/child grouping, counts and Git badges | `tray/src/SessionsWindow.cpp:3193-3348`; `tray/src/SessionsWindowRows.cpp:85-659` | Zerus Next merged list/filter projection; organization `.31`/`.47`-`.49`, archive `.40`/`.45`, unread `.59`, background count `.65` |
| Off-main-thread row snapshots, generation rejection, digest-based item updates, preserved selection/reveal and bounded transient rows | `tray/src/SessionsWindowRows.cpp:1-659`; declarations in `tray/src/SessionsWindow.h:101-167,230-256` | Zerus Next/Rust background projection and keyed list updates: merged implementation plus performance follow-up `.36`; no persistent worker migration |
| Live/archive inspection follow, bounded 500-event window, child inspection, read-only details/plan/goals/process/usage projections | `tray/src/SessionsWindow.cpp:3434-3716,3758-4201`; `tray/src/SessionsWindow.h:257-330,414-435` | Zerus Next Activity/side panel (`.2`, `.9`, `.10`), archived Activity `.42`, process inventory `.63`; `hgs` owns bounded journal/process reads |
| Composer/question delivery, local message reconciliation, attachments, voice and terminal file drops | `tray/src/SessionsWindow.cpp:687-1316,4386-4527,4805-5187,2618-2660` | Merged composer/question/attachment UI (`.9`, closed `.9.23.1`, `.9.24.1`/`.9.24.2`), with `hgs` exact identity and delivery receipts; voice/terminal behavior requires native acceptance |
| Deferred resume/fresh/compact/send, cold-cache consent, retry rules and 2.5-second continuation polls | `tray/src/SessionsWindow.cpp:4528-4627,4672-5002`; state in `tray/src/SessionsWindow.h:367-397` | **hgs** owns any delivery that must continue with the window closed, with immutable draft/identity/consent evidence and no uncertain replay; current client follow-ups do not establish a durable post-exit continuation owner: **gap** |
| Model/effort updates and one bounded automatic apply after a verified Ready transition; OpenCode catalog/favorite requests | `tray/src/SessionsWindow.cpp:4203-4385` | Model/effort and OpenCode picker work is `zerus-5wmh.69`; `hgs` owns pending settings identity/scheduling. Do not recreate a polling retry loop in the webview |
| Send now and Send and push now, native-queue text matching and 20-second push deadline | `tray/src/SessionsWindowShortcuts.cpp:147-340` | Merged queue/push in closed `.9.24.1`/`.9.24.2`; `hgs` owns native queue send-now identity, and push never resends message text |
| Visible-reply dwell/read acknowledgement, Mark all read, review-later/mute and attention count | `tray/src/SessionsWindow.cpp:3374-3433,5266-5449`; `tray/src/SessionsWindow.h:173-177,276-299` | Merged controls/read delivery (`.16`, closed exit flush `.53`) and attention counts (closed `.46`, `.58`); **hgs** owns post-exit delivery/badge state. Offline unread `.59`, background count `.65`; pending-read migration and durable worker are G3/G1 |
| Creation/legacy-launch cards and exact placement; task-executor launch reservation, persisted review state and at-most-once task delivery | `tray/src/SessionsWindow.cpp:1378-1475,2895-3127,3717-3757` | Zerus Next `.10.10`/`.10.10.1` and task UI `.10`; **hgs** owns launch/delivery journals. Migrate `workspace/pendingLaunches`, `tasks/pendingExecutorLaunches` and `.creations.json` read-only without replaying (**gap**) |
| Archive/terminate cleanup offers, owner nonce/result binding, background outcome status and manual review for retained/unknown folders | `tray/src/SessionsWindow.cpp:1734-1743,1916-1995,5543-5659,5758-5830`; cleanup declarations in `tray/src/SessionsWindow.h:312-327` | Zerus Next review UI (`.10`); **hgs** owns all cleanup after confirmation. Preserve ignored files/branches and never turn an unconfirmed result into deletion |
| Go to, recent/draft-owner navigation, shortcut help and platform/Terminal-safe command routing | `tray/src/SessionsWindowGoTo.cpp:1-213`; `tray/src/SessionsWindowShortcuts.cpp:1-444`; `tray/src/Shortcuts.cpp:13-375`; `tray/src/Shortcuts.h:10-85`; `tray/src/ShortcutsDialog.cpp:6-110`; `tray/src/ShortcutsDialog.h:7-20` | Keyboard editor/chords and Saved drafts navigation are merged (`.24`, `.9.34`), but `go.anywhere` and `help.shortcuts` have no consumers or popup/dialog; those are **gaps** in G7. Migrate customized `shortcuts/*`, including explicitly empty bindings |
| Background/foreground inspection, process and Git polling; 500 ms UI tick and delayed remote-loading indication | `tray/src/SessionsWindow.cpp:2014-2032,3467-3536`; `tray/src/SessionsWindow.h:556-575` | Interactive cadence belongs to Zerus Next/Rust subscriptions; merged no-stream peer polling is `client/core/src/legacy.rs` (closed `.34`); durable watch fanout/post-exit work remains **hgs** G1 |
| Versioned QSettings JSON cache with background parsing, byte/write guards and startup preload | `tray/src/SettingsJson.cpp:16-139`; `tray/src/SettingsJson.h:11-44` | Replace with Rust typed settings/journal decoding. One-time importer must bound and preserve unknown/malformed data rather than treating it as defaults |
| Settings navigation and Appearance/Sessions/Processes controls, plus embedding of recovery/update/mobile/files/worktree/approval/voice/keyboard/analytics/About sections | `tray/src/SettingsPage.h:34-219` | Merged Settings includes Show completed subagents, Usage limit indicator/Pace marker, Background process count, Unconfirmed process output, Show loading indicator after, and Open Zerus repository on GitHub; all those preference keys are seeded in `client/core/src/settings.rs` (the fixed repository action needs no seed). Other sections remain under their cited beads; About reads actual selected `hgs --version` |

### Single instance, tray helpers and Swarm

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Single-instance socket/lock, activation forwarding and systemd/launchd service-start waiting without killing another instance | `tray/src/SingleInstance.cpp:13-186`; `tray/src/SingleInstance.h:18-72` | Merged `client/src-tauri/src/instance.rs`, including Unix activation; macOS lifecycle `.44` and Windows native proof `.52` remain. The Linux retirement gaps are tray/menu/notifications in G4, not another single-instance implementation |
| Linux Status Notifier icon, activation, DBus-hosted menu and fallback message icons | `tray/src/SniTrayIcon.cpp:6-62`; `tray/src/SniTrayIcon.h:1-32` | Zerus Next Linux native tray integration: **gap**; remove KStatusNotifierItem dependency only after native activation/menu parity |
| Delayed nonactivating Windows startup notice while selecting/installing the CLI | `tray/src/StartupNotice.cpp:3-38`; `tray/src/StartupNotice.h:1-24` | Zerus Next native Windows startup feedback `.9`/`.52`; no durable state |
| Elided status text and visibility/reduced-motion-aware brand animation | `tray/src/StatusMessage.h:7-21`; `tray/src/SwarmButton.h:14-73` | Zerus Next shared UI, motion `.15` and minimized-motion `.39` |
| Swarm computer list, preview/join/project mapping, Sync now and settled/busy state | `tray/src/SwarmDialog.cpp:40-201`; `tray/src/SwarmDialog.h:7-10` | “Swarm…” is disabled in `Projects.tsx`; dialog/preview/join work remains open under `.18` and `.25`, with organization `.31`; `hgs` owns preview/join/sync and durable results |
| Exact causal-version conflict review and confirmed resolution for project/session/folder/machine fields | `tray/src/SwarmConflictReview.cpp:122-485`; `tray/src/SwarmConflictReview.h:12-48` | Conflict-review UI remains open under `.18`/`.25`; `hgs` replica owns raw heads/resolve. Preserve implicit/default placement and do not merge unseen versions |
| Serialized local organization edits, five-second reads, 120 ms flush, observed-version persistence and local-only Windows/fallback fields | `tray/src/SwarmController.cpp:8-225`; `tray/src/SwarmController.h:10-47` | **hgs** owns network/worker/catalog durability; Zerus Next edits via guarded host. Organization `.31`, no-GUI catalog `.48`; migrate `swarm/observed*`, `workspace/organizationBeforeSwarm` and all local-only fields read-only |
| Windows hidden `hgs swarm worker`, exponential restart and per-worker kill-on-close Job Object | `tray/src/SwarmWorkerSupervisor.cpp:11-162`; `tray/src/SwarmWorkerSupervisor.h:15-49` | **hgs**/a durable service owns catalog synchronization and restart, not the client lifetime. Linux/macOS units exist; Windows durable service/job handoff is **gap** |

### Tasks, task documents and recovery

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Task/stage/session/author projections, queue ranks, seven-day Done partition and consistent stage actions | `tray/src/TaskBoard.cpp:12-162`; `tray/src/TaskBoard.h:5-75`; `tray/src/TaskStageFilter.h:7-58`; `tray/src/TaskDrag.h:9-45` | Only the core is merged (`tasks.rs`, `task_view.rs`, `tasks_write.rs`; closed `.10.1`/`.10.2`); the board/inspector screen remains under `.10` and G7. `hgs task` owns canonical ordering, identities and mutations; drag payloads never become delivery authority |
| Board polling with visible/hidden cadence, generation guards, damaged/unreadable states and one follow-up read after mutation | `tray/src/TasksView.cpp:67-339`; `tray/src/TasksView.h:1-204` | Core reads exist, but the Tasks screen remains `.10`/G7 with bounded `hgs` reads. Replace window-owned polling with subscription where needed for background freshness |
| Guarded add/edit/rename/target/reorder/stage/start/cancel/settings operations and session-identity blocking | `tray/src/TasksView.cpp:341-796`; `tray/src/TaskBoard.cpp:133-162` | Core mutations are merged (`.10.1`/`.10.2`); the guarded UI remains `.10`/G7, and new-executor launch reservation remains durable `hgs` work |
| Task cards, stage filters, hover/focus actions, compact rows and in-progress animation | `tray/src/TaskRow.cpp:21-388`; `tray/src/TaskRow.h:6-92` | Tasks screen remains `.10`/G7; shared motion is `.15` and minimized-motion `.39` |
| Task detail/history/result/discussion, 100-card and 80-record windows, paged older history and 50-session assignment pages | `tray/src/TaskInspector.cpp:48-1064`; `tray/src/TaskInspector.h:90-141` | Core projections exist; Task View/inspector UI remains `.10`/G7. Legacy full-log replies are clipped immediately; retirement acceptance must use indexed history rather than grow a cache |
| Idempotent named-card creation and locally persisted title/new-card drafts | `tray/src/TaskInspector.cpp:900-1064`; `tray/src/TaskInspector.h:1-141` | `hgs task add --request-id` owns idempotency; migrate `tasks/newCard/*/request` and `tasks/titleDraft/*` without creating duplicates |
| Task description/comment composer, attachments, assignment editor, cancellation and project prompt settings | `tray/src/TaskDialogs.cpp:92-647`; `tray/src/TaskDialogs.h:15-154`; `tray/src/TaskContent.h:5-31` | Task editing UI remains `.10`/G7; shared draft import already parses `task-description` and `task-comment`, while legacy `tasks/noteDraft` forms and immutable attachments remain in G3 |
| Private per-task message recovery: 32 MiB bound, atomic snapshot/lock/marker, exact intent/dispatch/request IDs, read-only recover and explicit release | `tray/src/TaskMessageOutbox.h:9-65`; `tray/src/TasksView.cpp:654-756` | Merged client core has the same private outbox format and recover/release guards (`client/core/src/tasks_write.rs:433-629`); **hgs** must own reconciliation after exit. Migrate the QSettings marker and `.drafts/task-delivery/*.json` read-only; never submit a changed intent under an old identity |

### Terminal and process lifetime

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| External-terminal interface, direct program/argv and folder-shell behavior, plus one-shot advisories | `tray/src/TerminalBackend.h:9-59` | Zerus Next native terminal launcher (`.9`), with platform acceptance; terminals remain independent of client exit |
| libvterm cell parsing, alternate screen/reflow, 5,000-line/24 MiB scrollback, bounded repaint and title/bell | `tray/src/TerminalScreen.cpp:20-758`; `tray/src/TerminalScreen.h:8-91` | Rust terminal emulator/view in Zerus Next (`.9`); no persistent agent ownership moves to the view |
| Keyboard/IME/mouse input, selection/copy, sanitized bracketed paste and platform scrolling | `tray/src/TerminalScreen.cpp:425-758` | Zerus Next native terminal input, with native method coverage on every supported agent and OS |
| Explicit attach/detach/reconnect to `hgs a --existing`, exact run guards, terminal sizing and connection generation | `tray/src/TerminalView.cpp:15-208`; `tray/src/TerminalView.h:8-60` | Zerus Next Terminal (`.9`), with connection-scoped file drops. Detaching stops only the attach client, never the session |
| SIGTERM/SIGINT/SIGHUP self-pipe and normal quit/flush path, with ignored signals preserved and second signal able to terminate a hung quit | `tray/src/TerminationSignals.cpp:6-86`; `tray/src/TerminationSignals.h:13-40` | Merged `client/src-tauri/src/signals.rs` and `.9.30` (`exit_flush.rs`, `client/src/store/exitFlush.ts`). Dock Quit, logout or shutdown, other Cocoa `terminate:`, Windows `WM_ENDSESSION`, SIGKILL and Tauri restart keep the Exit/`exit_work` fallback without the page flush; the updater restart must call `quit_with_flush` (`.12.1`, `.12.3`). Service stops never signal native agents |

### Tray agent, menus and application lifecycle

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Local/peer discovery and adaptive polls, watch-v1 setup, menu/icon refresh, read-state settlement and JSON preload list | `tray/src/TrayAgent.cpp:41-592`; `tray/src/TrayAgent.h:1-137` | **hgs** owns durable fleet/watch/read workers and shared event fanout; Zerus Next presents interactive snapshots. Window-bound streams are not enough (**gap**) |
| Attention edge observation, post/withdraw/failure persistence, exact notification click routing and update-click separation | `tray/src/TrayAgent.cpp:163-202,552-609` | **hgs** owns durable intent/tokens and target resolution; native poster/activation on each OS. macOS posting `.43`, other platforms **gap** |
| Tray quick-open sessions, saved-session routing, Refresh, Open in, “Update available: Zerus N” and Quit; stable DBus menu items | `tray/src/TrayMenu.cpp:13-246`; `tray/src/TrayMenu.h:11-45`; `tray/src/TrayIcon.h:25-58` | Native menu UI is macOS `.44`; Linux/Windows menu and activation are G4. The update-available item is **gap** in G7: `.12.6` covers only Restart to finish update |
| Terminal/account/SSH/folder launch command construction and clipboard/open-mode preference writes | `tray/src/TrayAgent.cpp:616-873`; `tray/src/TerminalBackend.h:31-59` | Zerus Next native launcher and Settings; `hgs` receives exact launch identity, while existing detached native agents are never stopped |
| Non-Windows desktop presence and Windows swarm worker startup | `tray/src/TrayAgent.cpp:257-296` | **hgs** durable presence and catalog services, not a hidden client (**gap**) |
| Tray icon abstraction and clickable/picture message fallback | `tray/src/TrayIcon.h:25-58`; `tray/src/TrayAgent.cpp:797-830` | Zerus Next native notification/tray adapters: macOS `.43`/`.44`, Linux/Windows **gap** |
| Optional Russian translator with fallback and language preference | `tray/src/UiLanguage.h:14-51` | Zerus Next i18n `.11`; migrate `workspace/language` read-only |
| Process startup: CLI modes/build info/selftest, config/CLI selection, single instance, translation, tray availability, watchdog, activation and update-on-start | `tray/src/main.cpp:59-444` | Zerus Next native executable and service integration (`.12`, `.44`, `.52`). Preserve `hgs` commands and existing application/service identifiers |
| Windows Unicode argv, parent-console text, stable CLI staging, INI settings, per-user named-pipe security and restart registration | `tray/src/main.cpp:59-142,167-252,298-411` | Zerus Next Windows native host (`.52`); do not replace these with a webview or shared cross-user pipe |

### Updates, platform integration and shared workspace widgets

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Verified stable/nightly schema-1 and Dev schema-2 feed parsing, bounded downloads/caches, release/channel identity and notification dedup | `tray/src/UpdateController.cpp:30-202,296-334,481-652`; `tray/src/UpdateController.h:1-155` | Rust update service in Zerus Next (`.12.5`), with `hgs` or native durable scheduling for checks with the window closed (**gap**). Feed compatibility must not add Windows/macOS assets to v1 artifact kinds |
| Installation ownership and update instructions for AUR/source/Homebrew/DMG/winget/portable Windows, package comparisons and channel changes | `tray/src/UpdateController.cpp:147-295,319-446,540-638` | Zerus Next Updates UI (`.12.4`), copying commands only. Preserve installed build identity and never stop agents/tmux |
| macOS LaunchAgent repair and detached restart-after-exit helper | `tray/src/UpdateController.cpp:430-539` | Zerus Next macOS updater (`.12.1`, `.12.2`, `.12.6`), preserving drafts and compatible privacy identity |
| Updates screen: channel/automatic controls, check state, release/download links and copied installation commands | `tray/src/UpdatesWidget.h:15-217` | Zerus Next `.12.4`; migrate `updates/channel`, `updates/automatic`, cached catalogs/timestamps and notification dedup without reposting old updates |
| Keep-above preference, native non-Wayland flags and confirmed KWin scripting/probe/retry | `tray/src/WindowLayer.cpp:10-106`; `tray/src/WindowLayer.h:7-34` | Zerus Next `.24.2`; migrate `workspace/alwaysOnTop` without overriding compositor refusal |
| Windows taskbar badge and clickable attention toast with stale-click withdrawal | `tray/src/WindowsApplicationBadge.cpp:4-27`; `tray/src/WindowsAttentionNotifier.cpp:8-51` | **hgs** count/intent plus Zerus Next Windows badge/toast integration: **gap** |
| MSVC `CreateProcess` argv encoding and visible console allocation policy | `tray/src/WindowsCommandLine.h:10-52`; `tray/src/WindowsConsole.h:8-37` | Zerus Next Windows launcher/PTY (`.9`, `.52`) |
| Windows legacy connector panel with private credential replacement and revision/generation-confirmed lifecycle | `tray/src/WindowsMobileSettings.h:15-146` | Zerus Next mobile/legacy UI (`.24.5`, `.24.8`); connector owns credentials/journal/service, never QSettings/webview |
| Windows Terminal tab/window/console fallback, exact argv preservation and advisory | `tray/src/WindowsTerminalBackend.cpp:14-202`; `tray/src/WindowsTerminalBackend.h:11-61`; `tray/src/ConsoleHost.cpp:10-168`; `tray/src/ConsoleHost.h:9-42` | Merged native launcher and `client/core/src/terminal_launch/console_host.rs`; Windows proof `.52`, packaging needs a non-Qt console-host entry point |
| Windows self-update trust anchors: ECDSA manifest verification and Authenticode publisher with full-chain revocation checking | `tray/src/WindowsUpdateTrust.cpp:11-88`; `tray/src/WindowsUpdateTrust.h:6-27` | Zerus Next updater (`.12`), with `zerus-qxdx.1` owner provisioning. Both anchors are empty at baseline, so every build remains link-only and downloads execute nothing |
| Workspace connector create/join/change, pending migration checks, invitations, lifecycle and private status validation | `tray/src/WorkspaceSettings.h:27-310` | Zerus Next `.24.5`-`.24.8`; `hgs`/connector owns private configuration and operations, not the UI process |
| Shared icon/button/list/header/style/color-popup primitives | `tray/src/WorkspaceIcons.h:10-254`; `tray/src/WorkspaceList.h:7-31`; `tray/src/WorkspacePageHeader.h:8-44`; `tray/src/WorkspaceStyle.h:5-32`; `tray/src/WorkspaceColorDialog.h:8-35`; `tray/src/WorkspaceFocus.h:1-49` | Zerus Next shared UI/accessibility (`.11`), no durable state |

### Worktrees and destructive review

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| On-demand folder/worktree catalog, bounded 512 rows, session associations, offline snapshots and five-minute reminder reads | `tray/src/WorktreePanel.h:22-162` | Zerus Next Worktrees (`.10`); `hgs` owns bounded catalog reads and evidence |
| Shared cleanup categories, integration/local-data/process rules and exit-cleanup eligibility | `tray/src/WorktreeReviewUi.h:11-110` | Rust policy/projection shared with `hgs`; age/name never authorizes removal |
| Per-computer automatic-cleanup policy editor with help capability probe, revision validation and no implicit maintenance | `tray/src/WorktreeSettings.h:16-85` | Zerus Next Worktree Settings (`.24`); `hgs` already owns durable maintenance and malformed policy fails closed |
| Project-wide overview, two catalog reads, one-hour cache, five-minute cleanup proof and filters/sorting/selection | `tray/src/WorktreesDialog.cpp:75-526`; `tray/src/WorktreesDialog.h:71-81` | Zerus Next overview (`.10`), bounded/indexed background reads; cache snapshots are not ownership evidence |
| Bounded changed-file/diff/commit/local-file/related-session inspection with generation/fingerprint guards and paged previews | `tray/src/WorktreeInspectionPanel.h:20-161` | Zerus Next inspector over `hgs` bounded reads; no full-repository/file loading into webview memory |
| Single/batch reviewed removal, exact fingerprints, ignored/untracked/process consent and merged-branch options | `tray/src/WorktreeCleanupDialog.h:19-211`; `tray/src/WorktreeBatchCleanupDialog.h:19-226` | Zerus Next confirmation UI; **hgs** performs removal on the owning machine with exact fresh evidence |
| Bounded background termination outcomes (64 targets), kept/unknown review and no unknown replay | `tray/src/WorktreeCleanupStatus.h:13-87` | Merged outcomes in `client/core/src/termination_cleanup.rs` (`.10.6`), with `worktree_review.rs` (`.10.5`) and `worktrees.rs` (`.10.4`); **hgs** post-exit ownership and outcome handoff remain G2/G3, with no unknown replay |

### Voice, video and speech providers

| Responsibility and kind | Qt source | Retirement target and coverage |
| --- | --- | --- |
| Circular video preview, coalesced frame conversion, camera/microphone consent, format selection and at-most-60-second/10 MiB recording | `tray/src/speech/RoundVideo.cpp:12-162`; `tray/src/speech/RoundVideo.h:10-67` | Zerus Next UI plus `client/media` (`.9`, `.9.19`); recording stops on cancellation/hide, with private storage and native device acceptance |
| Video audio extraction, PCM preparation, poster generation and cancellation across worker boundaries | `tray/src/speech/RoundVideo.cpp:120-162` | `client/media` and speech service; no Qt multimedia dependency |
| Bounded speech/voice CLI requests with per-host capability probes and management-error redaction | `tray/src/speech/SpeechClient.cpp:9-107`; `tray/src/speech/SpeechClient.h:11-26` | Rust media/speech host calls to existing `hgs speech`/`voice` commands |
| Voice/video composer state, durable WAV/MP4 drafts, provider selection/revisions, exact send/check/cancel identity and rejection proof | `tray/src/speech/SpeechComposer.cpp:36-583`; `tray/src/speech/SpeechComposer.h:15-103` | Zerus Next composer (`.9`) with `hgs` durable voice requests; migrate `voice-message-draft` records and `speech/*` preferences without replaying audio |
| Draft-only transcription with bounded 120 polls and insertion guarded by draft identity | `tray/src/speech/SpeechComposer.cpp:537-583` | **hgs**/speech service owns job status after exit; UI inserts only after fresh draft match, no duplicate audio submission (**gap** if client polls are still window-bound) |
| Streaming audio conversion to bounded two-minute 16 kHz PCM, worker capture, permission checks and device errors | `tray/src/speech/SpeechRecorder.cpp:19-188`; `tray/src/speech/SpeechRecorder.h:10-47` | `client/media` with native microphone integration; preserve off-UI conversion and never request consent during selftest |
| Speech provider settings/stats, profile editor/credentials and revision-confirmed add/edit/remove/test/default | `tray/src/speech/SpeechSettings.cpp:29-399`; `tray/src/speech/SpeechSettings.h:15-49` | Zerus Next Voice Settings (`.24`); provider credentials stay on processing computer. Migrate `speech/providerOperation` for read-only status recovery, not mutation replay |
| Static delivery/transcript icons, video posters and waveform rendering | `tray/src/speech/VoicePlayerImage.h:13-107` | Zerus Next media presentation; no persistent owner |

## Installers, service units and distribution packages

| Responsibility and kind | Qt-side source | Retirement target and coverage |
| --- | --- | --- |
| Atomic CLI build/install plus opt-in mobile connector and independently managed recovery/swarm/session/voice services | `install.sh:1-117` | Retain `hgs` installers and their no-agent-restart behavior. Add only the durable services required by this spec; no Qt removal step may restart native agents |
| Linux source GUI build, binary/plugin install, icons, desktop entry, systemd enable and guarded live replacement | `tray/install.sh:206-344` | Build/install Zerus Next instead of CMake/Qt, remove `libhgs-tray-native-ui.so` and obsolete Qt payloads, retain maintained updater and service identity |
| macOS source GUI choice/build/sign/bundle/LaunchAgent replacement | `tray/install.sh:26-204`; `mac-install.sh:119-142` | Complete `.12.2` switch packaging, then make Zerus Next the only GUI source. Preserve bundle identifier, privacy-compatible signing, `hgs` and connector |
| Remote Mac CLI/tmux source installation and forwarded GUI selection | `mac-install.sh:1-142` | Keep CLI and tmux installation unchanged. Ship only required Zerus Next sources after retirement; no Linux build products or Qt caches |
| Guarded GUI replacement with draft snapshots, native-process identity checks, one rollback copy, build provenance and Dev-number guard | `tray/update-installed.py:159-1105` | Retain the maintained updater and its fail-closed behavior. Extend it for final Qt executable/plugin cleanup only after migration and native acceptance; Qt settings/drafts are not deleted as part of GUI removal |
| Linux GUI systemd unit: graphical-session start, crash restart, no tray exit suppression and `KillMode=process` | `tray/packaging/hgs-tray.service:1-18`; installed by `tray/install.sh:249-323` and `scripts/package-linux.py:56-58` | Preserve service label and native-agent lifetime, point `ExecStart` at Zerus Next. Durable `hgs` workers get separate units rather than relying on this GUI process |
| macOS LaunchAgent: identity, explicit CLI path, interactive priority, crash-only restart and bounded log path | `tray/packaging/com.hgdev.hgs-tray.plist:5-43` | `.12.2`/`.44` retain label and semantics. Only the client starts at login and owns the menu-bar item; native services remain independent |
| macOS bundle identity, display naming, UI-element mode and camera/microphone usage descriptions | `tray/packaging/Info.plist:5-24` | Replace through `Info.switch.plist` in `.12.2`; preserve privacy identity and required entitlements without Qt Frameworks/PlugIns |
| AUR stable/VCS source builds, Qt/runtime dependencies, native plugin and license staging | `packaging/aur/zerus/PKGBUILD.in:9-62`; `packaging/aur/zerus-git/PKGBUILD:9-67`; `scripts/package-linux.py:21-101` | Replace GUI build/package inputs with Zerus Next and its actual native dependencies/notices. Remove Qt base/multimedia/svg/WebEngine/KF6 dependencies and native plugin only after Linux acceptance |
| AUR prebuilt stable/nightly packaging and verified library floors | `packaging/aur/zerus-ade-bin/PKGBUILD.in:9-29`; `packaging/aur/zerus-ade-nightly-bin/PKGBUILD.in:9-29` | Recompute `.BUILDINFO` floors from the Zerus Next package; remove Qt optional dependency when nothing needs it. Preserve channels, version identities and immutable published assets |
| Homebrew casks: signed Apple Silicon app, CLI/setup links and opt-in service cleanup | `packaging/homebrew/zerus.rb.in:1-44`; `zerus@nightly.rb.in:1-46`; `zerus@dev.rb.in:1-46` | Ship the Tauri `Zerus.app` through `.12`. No `macdeployqt`, Qt frameworks, Qt SBOMs or Qt runtime requirement; retain cask no-quit/no-service-restart behavior and private connector journal |
| Windows portable payload: CLI, GUI, connector, ConPTY, app-local runtime, README and notices | `scripts/package-windows.py:108-235`; `packaging/windows/README.txt:1-84`; `packaging/windows/conpty.json:1-20`; `packaging/windows/qt.json:1-77` | Replace `hgs-tray.exe` with Zerus Next, retain `hgs.exe`, connector and ConPTY, remove Qt deploy/SBOM/source pointers and only genuinely unused runtime files. Keep layout/resource resolution compatible |
| Per-user Windows installer: stable AppId, close only GUI processes using its folder, payload-driven stale-file removal and no state deletion | `packaging/windows/zerus.iss:32-85,174-269`; `scripts/package-windows-installer.py:1-86` | Update launch entry points and payload while retaining AppId and safeguards. A version without Qt must remove obsolete Qt DLLs from its own folder only, never CLI/state/drafts |
| Stable/nightly v1 feed and Dev v2 feed validation/publication | `scripts/update_feed.py:212-360`; `scripts/dev_updates.py:41-190`; `docs/public-distribution.md:52-259,360-473` | Same artifact kinds and compatible catalogs can carry the new packages after their contents qualify. Windows zip stays GitHub-only/out of v1 and absent from Dev; macOS downloads stay outside the artifact array |
| winget portable manifest and zip layout | `scripts/winget_manifest.py:35-81`; `docs/public-distribution.md:382-401` | Preserve `ufna.Zerus`, immutable stable-only publication and `zerus-windows-x86_64/hgs-tray.exe` unless a separately reviewed migration changes the launcher contract. PATH dependence remains while resources are app-local |
| Update catalog metadata and client feed parser | `tray/src/UpdateController.cpp:30-202`; `scripts/dev_updates.py:41-190`; `scripts/update_feed.py:212-296` | `qt_version` currently participates in macOS provenance validation. Introduce runtime-neutral metadata compatibly at producer and consumer before dropping it; never publish a catalog old clients misread |

## Gap beads

G7 is `zerus-5wmh.66.2`; G1-G6 are `zerus-5wmh.66.3` through `zerus-5wmh.66.8` respectively. Existing beads named in the tables remain necessary; this list covers responsibilities for which no merged implementation or existing scoped bead was found. The parent is `zerus-5wmh.66`. The dependencies are **G3 before G5 and G6**, and **G1 and G2 before G6**; G4 and G7 must also satisfy the platform acceptance they affect before removal. Proposed owners follow the owner's split of UI/design to Claude and technical work to Codex/Kimi; they are not assignments, and G1-G6 currently remain unassigned.

### G1: Durable fleet, presence and notification services in `hgs`

**Proposed owners:** `codex/zerus-work-431af08b/core` with `codex/zerus-work-6768c5c5/a2hub`.

Move the window-independent parts of `EventStream`, `TrayAgent`, `DesktopPresence`, `AttentionTracker`, `ReadMarkSync` and `AutomaticApprovals` into one bounded service architecture: local/peer watch fanout, owner idle/lock sampling, attention tokens and posting intent, read-mark retries, durable attention counts and native routing. Include `.54` and `.65` rather than duplicating them.

**Decision — service topology (owner, 2026-10-29):** option (a). One new durable per-user `hgs desktop` service, installed as a systemd user unit, a LaunchAgent and a Windows startup registration. It has one bounded event fanout and explicitly partitioned writers, one writer per durable intent, and is isolated from the `com.hgdev.hgs-recovery` and `com.hgdev.hgs-voice` services and the swarm worker. It uses bounded indexed reads and resources, runs no duplicate pollers or notification posters, and its maintained upgrades preserve running native agents and never restart recovery or voice work. Option (b), extending the existing swarm worker and the recovery and voice services, was not chosen. Recorded on `zerus-5wmh.66` and as the Beads memory `qt-retirement-g1-topology`. The G1 design document goes to `claude/zerus-work-e94411a3/webui-ac` for review before implementation.

Acceptance: closing every client leaves correct alerts/read delivery/badge state working on Linux, macOS and Windows; no second poller or duplicate notification appears after reconnect or upgrade.

### G2: Durable operation and recovery owners

**Proposed owners:** `codex/zerus-work-431af08b/core` with `codex/zerus-work-6768c5c5/a2hub`.

Give `hgs` the post-exit lifecycle for creation/jobs and uncertain-result reconciliation: `.creations.json`, `workspace/pendingLaunches`, `tasks/pendingExecutorLaunches`, deferred resume/compact/send, task-message recovery, draft-only transcription, provider operations, download-cache maintenance and termination-cleanup outcomes. Preserve exact request/consent identities and expose read-only status/recovery to Zerus Next. Preserve the `.9.30` exit boundary: Dock Quit, logout or shutdown, other Cocoa `terminate:`, Windows `WM_ENDSESSION`, SIGKILL and Tauri restart keep the Exit/`exit_work` fallback without the page flush; the updater restart must call `quit_with_flush`. Acceptance: each operation finishes or remains explicitly reviewable after client exit; none repeats an uncertain clone, launch, send, answer, transcription or destructive removal.

### G3: One-time read-only Qt and interim-store migration

**Proposed owners:** `codex/zerus-work-431af08b/core` with `codex/zerus-work-6768c5c5/a2hub`.

**Extend** the merged importers; do not create a second migration stack:

- `client/core/src/settings.rs` (`QtStore::here`, `client/core/src/settings/qt.rs`) already reads Linux `~/.config/hgdev/hgs-tray.conf`, Windows `%APPDATA%\hgdev\hgs-tray.ini`, and macOS defaults exports/plists.
- `client/src-tauri/src/settings.rs` seeds once only under `SWITCH_IDENTITY` (`com.hgdev.hgs-tray`, with `identity.rs`); the dev identity never seeds. That switch identity exists only for macOS today, so Linux and Windows still need their switch identity and one-seed rules.
- `client/core/src/attention.rs` `seed()` reads Qt's `attention/observed` once and prefers the client's own file.
- Qt v1 drafts already import read-only through `client/core/src/drafts.rs` and `import_at_start` (`client/src-tauri/src/lib.rs:349`; closed `.13.1`, ordered store `.9.11`), including `question\n`, `task-description` and `task-comment` keys.
- `.40` seeds `run_groups` read-only into local `organization-layout.json`, capped at 20,000 entries with oldest written first; `.47` uses the same local layout for at most 100,000 run-token members.

The existing Settings reader/writer now records versioned first-G3 imports for
`machines/colors|vivid`, `allSwarmProjects` and `collapsedSessionRows`, with
private quarantine metadata and retries for failed families. Row density keeps
Qt's bounded packed hashes, matches exact host/raw-command/archive/conversation
identity and persists new choices without rereading a handled Qt family.
`client/core::migration` supplies bounded staged evidence over the existing
creation/read/task/launch decoders, separate repository-clone checkpoints,
download indexes, organization observations/backups/drafts, provider operations
and non-authoritative worktree caches, plus original draft-operation manifests
and attachment references through the existing draft decoder. Historical Qt
and Next recovery-policy caches use the existing envelope decoder, retaining
policy/order/unknown fields without reconciliation or execution. Qt and interim
Next attention-observed history also has private bounded evidence through the
existing seed parser, without notification delivery, withdrawal or token replay.
Qt and Next local attention marks/read replies also retain original keys,
unknown fields and pair order through the existing mark/file decoders, without
applying, settling or converting them. Original states, identities and private
unknown fields are preserved without replay. Its transport
client streams to G1's selected publisher with exact capability/configuration checks, bounded pipes
and source rechecks; it has no startup hook. The shared handoff contract lives
in `crates/desktop-migration`; G1 owns the executable transport, secure publisher,
marker phases and current drain. The first-G3 `task-preferences` family also
imports a previously unhandled missing `tasks/onlySession` value once.

Retained current Qt shared organization also recovers missing or
`imported=false` catalogs through the existing missing-field initializer;
existing causal values and tombstones win. The separate first-G3 local
`organization-presentation` family records collapsed projects, session order
and default project together in the existing local layout writer. Only these
three fields may fill empty/null Next defaults under the coordinator's explicit
2026-10-30 cosmetic exception; nonempty Next fields win, and handled families
and the earlier run-memory seed never repeat. Both decoders share Qt's bounded
legacy `groups` and `before_pin` restoration rules, preserving exact project
and session identities without generating missing folder IDs or discarding an
ambiguous bucket. With the approved Soft/Bright fill UI consumer (`.66.2.30`)
merged, first-G3 `project-vivid` (`.66.5.3.9`, schema 1) uses that same decoder
and Settings writer to seed explicit boolean flags once into local
`projects/vivid/<exact-project-id>`. Existing Next values including false win;
missing flags do not create defaults. The atomic family record keeps the
original organization key and Qt store hash/path/time/mtime condition. Invalid
or ambiguous candidates retry without a partial family or a handled record;
handled families never repeat after rollback. Original private evidence stays
unchanged and vivid never enters shared organization or local layout.
Local Windows folders use the existing `local_windows_folders` Layout family.
The switch app calls its bounded Qt preparer on the organization worker after
a current local Windows watch and matching catalog are adopted. Explicit
original folder machine IDs must match that node; aliases and path spellings
never establish ownership. Existing whole-family presence (including empty or
unsupported values) wins before Qt is read. A fresh catalog/source check and
current live project IDs are required at the existing Layout writer commit.
Folders and handled metadata share one checkpoint; absent Qt organization stays
unhandled without a repeated error or fabricated empty family. This local
presentation import does not enable native folder actions or change shared data.

The 2026-11-01 release assignment supersedes the earlier deferral of the
Projects splitter. The first-G3 `project-folder-splitter`, `session-splitter`
and `window-geometry` families decode bounded Qt saved states into the
matching Next Projects split, Sessions list width and main-window normal
bounds/maximized/fullscreen state. They commit values and family metadata
through Settings, once, with every stored Next value winning. Unknown versions
or malformed/truncated data remain quarantined/unhandled and preserve Qt.
Next's former browser-stored split widths move through its existing migration
before loading the Qt seed, so prior Next choices still win. Window bounds
restore within an available display and subsequent moves use Settings; this
never writes Qt. `tasks/inspectorSplit` still has no equivalent Tasks splitter
and remains private typed evidence; it never maps to another inspector panel.
The remembered
`removeWorktreeAfterTermination` value is retained as private evidence and now
prefills the `.10.17` Terminate checkbox through one `termination-default` G3
family. Import only when Next has no stored value: a current false wins. The
family uses the same source hash, mtime, key and one-time handled record as the
existing importer. Every Terminate still needs explicit confirmation, and this
remembered default never authorizes automatic maintenance or a removal alone.

The shared `tray.conf` also has bounded private evidence through its existing
key parser; it preserves the original text without changing the selected CLI,
terminal, polling or service settings. Downloaded content stays untouched in
its existing shared cache folder; migration copies no content or edited/unknown
files. Remaining production integration covers the interim stores listed below. Linux
notification owner/token and Next unknown-delivery data also have bounded
private evidence through G4's shared Saved decoder/validator, retaining original
fields separately from the typed projection. The existing secure native reader
and one-time seed remain unchanged; runtime worker/lease admission stays separate.
Source discovery and production integration remain separate; adapters do not
by themselves publish eligible evidence or transfer writer authority. The
`approval_policies` adapter inspects Qt and interim Next choices together,
preserves unknown Next policy details privately and emits no consent. Source
discovery, production publication and the service's approval-reset presentation
remain separate integration work.

Acceptance: repeated import is idempotent; every source stays unchanged; unknown/malformed fields survive in private quarantine; no dangerous feature auto-enables; no old alert/message/request replays; the client writer stops before the `hgs` writer starts.

### G4: Native Linux and Windows shell integration; macOS durable intent

**Proposed owner:** Kimi for native integration.

Provide native activation, tray/menu/badge, Freedesktop and Windows attention posting/routing, Linux clipboard fallback, Windows startup/restart behavior and the Windows durable swarm worker; reuse merged single-instance and console-host code rather than replacing it. Complete `.43`/`.44` on macOS but originate intent in `hgs` rather than only while the app runs. The preserved `com.hgdev.hgs-tray` app-bundle identity posts native notifications; `hgs` keeps the durable intent.

Acceptance: primary-click/session/update routes, withdrawn stale clicks, accessible menus and correct icons work without Qt; Linux no longer requires KStatusNotifierItem/KWindowSystem/KIdleTime. macOS acceptance explicitly includes alert posting and click routing through the LaunchAgent path while Zerus Next is closed.

### G5: Zerus Next packaging and Qt dependency removal

**Proposed owners:** Kimi with `codex/zerus-work-431af08b/core`.

Replace GUI inputs in AUR, Homebrew, Windows zip/installer and source installers with Zerus Next; add the durable service units chosen in G1; update licenses/SBOMs and runtime-neutral release metadata; retire Qt WebEngine and KF6/Qt dependencies. Preserve CLI, application/service IDs, `HGS_*`, channel/version identities, feed artifact kinds and winget layout. Do not touch the `zerus-mobile-connector` unit or `~/.local/share/zerus-mobile` before `zerus-nmc8`; moving a live gateway requires owner approval. Acceptance: clean install and upgrade on each platform contain no Qt runtime requirement and launch the same accounts/projects/sessions, while previously published assets remain immutable and running native agents stay alive.

### G6: Retirement acceptance, native parity and removal gate

**Proposed owner:** `kimi/zerus-work-ba0ed9fd/events-k` for fixtures/harnesses, with `claude/zerus-work-e94411a3/webui-ac` as gatekeeper.

Create shared contract fixtures for every inventory group, native Linux/macOS/Windows integration runs, read-only migration/interruption tests and a final source/package audit proving every `tray/src` responsibility has a target. Run real terminal/native agents, notification clicks, background workers, voice capture and destructive-review refusal cases. Only after all G1-G5 and G7 evidence is green for a platform may its maintained installer remove the Qt executable, rollback offer and obsolete plugins; keep original user data unless the owner separately approves deletion. Windows acceptance remains blocked on access to the Windows PC as recorded in the phasing section.

### G7: Zerus Next UI gaps (`zerus-5wmh.66.2`)

**Design rulings:** `claude/zerus-work-e94411a3/webui-ac`; **implementation:** Codex/Kimi sessions under the owner's split.

Implement the five remaining UI gaps: the Go to popup and shortcut-help dialog; live session-content and archive-content search; the Tasks screen under `.10`; Hide launch status (`tray/src/SessionsWindow.cpp:505`); and the tray's “Update available: Zerus N” item (`tray/src/TrayMenu.cpp:164`). `.18`, `.25` and `.69`-`.72` remain separately cited work, not parts of this package. Sort sessions by name, detailed seeded Settings controls, per-machine Pause all/Resume all, Show running commands, the composer duplicate safeguard and resize handle, and Copy connect/restore command are merged and mapped in the inventory, so G6 covers their acceptance without duplicating them here.

## Migration contract

1. **Read-only sources.** Never write QSettings, exported macOS preferences, `tray.conf`, Qt draft/creation/task/outbox stores, Zerus Next interim stores or connector journals during import. Use bounded reads, tolerate atomic replacement, preserve original bytes and use a private report with no message content, audio, tokens or personal audit data.
2. **Single writer after handoff.** `hgs` owns durable operation/journal state; Zerus Next owns presentation and local UI-only preferences. Record an explicit handoff marker: the client writer must stop before the `hgs` writer starts, and two writers must never run. Existing client import code is reused, not treated as proof of the missing durable owner.
3. **Identity before defaults.** Import exact run/conversation/launch/archive/request/checkout ownership and causal versions first. Missing or conflicting evidence stays for review; never infer it from a display name, lexical path, timestamp or current filter.
4. **No replay or privilege expansion.** Recovery uses read-only status/receipt checks. Automatic approvals remain off (Qt's former choice may be reported), and no notification, message, task, voice, cleanup or provider mutation is repeated automatically. **Microphone is not imported; the user chooses again**, because Qt's `QAudioDevice` ID names no client device.
5. **Fail closed and preserve.** Invalid/truncated/unsupported records are quarantined, not dropped or rewritten as defaults. Re-running an importer is idempotent. Branches, ignored files, edited downloads and unknown local data remain untouched.
6. **Rollback is not a new seed for a handled family.** The switch seed's settings, layout and drafts remain handled and never re-seed after rollback. Owner/coordinator ruling of 2026-10-30 permits one explicitly scoped, versioned first-G3 import of each previously unhandled family. Import only keys Next has not stored. The coordinator's narrow local-presentation exception permits `collapsed`, `order` and `default_project` to fill empty/null Next defaults once; nonempty Next values win and every other family retains strict key-presence precedence. Record the family, schema, source path, time, key names and hash of the bytes read. A source changed since the original switch is still eligible for a new family; record that observation without treating it as an error. A failed or partial family stays unhandled and retries at the next start. Later G3 versions may import only new families. Continued read-only access to Qt drafts through Saved drafts “From the previous Zerus app” (`client/src-tauri/src/composer/saved.rs`, `SavedDrafts.tsx`) and Recover does not re-import or replay them.
7. **Local layout stays local.** The client's `organization-layout.json` is a local per-device store like Qt's local QSettings, not a synchronized second format. Shared organization uses the `hgs` replica; `.40`/`.47` local run memory and tokens do not create another sync protocol.
8. **Connector boundary.** Connector settings import is read-only. Retirement never touches the `zerus-mobile-connector` unit or `~/.local/share/zerus-mobile` before `zerus-nmc8`; moving a live gateway is owner-approved.

### Persisted data checklist

| Data family | Required handling |
| --- | --- |
| Workspace appearance and behavior | Existing switch seeds include activity/content scaling, theme/language, detailed Sessions/Processes controls and updates/downloads preferences. Read-only one-time imports cover launch defaults, collapsed rows, project folder splits, the termination cleanup choice and matching window geometry/splitters; unsupported shapes remain preserved. Never convert a UI preference into a background opt-in |
| Organization and Swarm | Preserve `workspace/organization`, `organizationBeforeProjects`/`BeforeSwarm`, `swarm/observedOrganization`/`observedVersions`, imported machines, explicit/unassigned placement and local Windows paths. Keep `.40`/`.47` run groups/tokens in local `organization-layout.json`; reconcile only shared organization through the `hgs` replica |
| Zerus Next interim stores | Migrate the switch identity's `settings.json`, draft store, `local_messages`, attention file, `organization-layout.json`, creation records (`.10.9`, `.55` ended launches), approvals journal, termination-cleanup outcomes and download cache. Read once, idempotently and without replay; honor the writer-handoff marker before `hgs` takes ownership |
| Drafts and forms | Extend the existing Qt v1 read-only import for message, child, question, voice/video and task description/comment/draft forms, cursors/form state and content-hashed immutable attachments. Qt drafts remain readable after rollback through previous-app Saved drafts/Recover; failed/uncertain recovery never submits |
| Creation and launches | Import `.creations.json`, `workspace/pendingLaunches` and `tasks/pendingExecutorLaunches`; resume only by exact durable receipt and leave unknown outcomes reviewable |
| Tasks | Preserve `tasks/newCard/*/request`, `titleDraft`, `noteDraft`, `messageRequest`, `onlySession`, `inspectorSplit` and `.drafts/task-delivery/*`, keeping request IDs distinct from task UUIDs |
| Attention and reads | Extend the existing `attention/observed` seed to `sessionMarks`, `readReplies`, `pendingReads`, `sharedReadMigrated`, Linux notification owner/token maps and update dedup without re-posting historical alerts; hosts remain authoritative for shared reads |
| Repository/worktrees/downloads | Preserve `workspace/repositoryClone/*/pending`, worktree snapshots as non-authoritative caches, `.zerus-files.json`, edited/unknown files and `downloads/*`; no cache trim or cleanup during import |
| Recovery, tasks, speech and updates | Preserve `recovery/sharedPolicy`, speech/provider operation identities and preferences, task forms, and update channel/catalog/timestamps. Provider API keys and relay credentials stay only in their existing privileged stores |
| Microphone | Do not import `speech/microphone`; the user selects the client device again |
| Startup and keyboard | Read `tray.conf`, `shortcuts/*`, `processes/*`, `machines/colors/*`, `machines/vivid/*`, `automaticApprovals/enabled` and service startup choices without changing the selected CLI, enabling sessions or replacing service definitions blindly |
| Mobile connector | Settings import is read-only; leave the connector unit, credentials, journal and `~/.local/share/zerus-mobile` untouched before `zerus-nmc8`, with any live-gateway move separately owner-approved |

## Acceptance and existing Qt test coverage

At baseline, `tray/tests` has **98 tracked files**. `tray/tests/CMakeLists.txt` registers offscreen suites with fatal GUI-thread JSON checks; `support/TestIsolation.cpp` forces per-process INI settings and an isolated fixture pool on Windows, while `support/FakeCli.h` and `support/FakeTasksCli.h` provide bounded fake CLIs, idempotent task requests and uncertainty injection. Retirement tests must move these contracts to the owning Rust/native layer rather than merely deleting them with Qt.

| Inventory area | Existing evidence to preserve | Additional retirement evidence required |
| --- | --- | --- |
| Accounts/fleet/read state | `test_accountfilter.cpp`, `test_accountspage.cpp`, `test_accountusage.cpp`, `test_fleetstate.cpp`, `test_readmarksync.cpp`, `test_attention.cpp` | Durable post-exit presence/read workers and offline/unread/migration acceptance on all platforms |
| Activity/composer/questions | `test_activityview.cpp`, `test_messagecomposer.cpp`, `test_composertoolbar.cpp`, `test_questioncard.cpp`, `test_markdownhtml.cpp`, `test_markdownobjects.cpp`, `test_attachmentviewer.cpp` | Exact draft migration, post-exit sends/answers, archived read-only activity and native media/attachment acceptance |
| Creation/session/project UI | `test_sessioncreation.cpp`, `test_newsessiondialog.cpp`, `test_projectsdialog.cpp`, `test_directorydialog.cpp`, `test_clonerepository.cpp`, `test_sessionsearch.cpp`, `test_dashboard.cpp`, `test_machinefilter.cpp`, `test_machinespage.cpp` | Durable creation recovery, organization/run-token import and direct-folder/custom/existing-worktree launch on every platform |
| Main window/shortcuts/tasks | `test_sessionswindow.cpp`, `test_shortcuts.cpp`, `test_gotodialog.cpp`, `test_keyboardsettings.cpp`, `test_taskboard.cpp`, `test_tasksview.cpp`, `test_taskdialogs.cpp`, `test_taskdrag.cpp`, `test_opencodemodelpicker.cpp` | Task journal/outbox migration, deferred delivery owner, keyboard/native terminal acceptance and bounded large-catalog behavior |
| Worktrees/processes | `test_worktreepanel.cpp`, `test_worktreesettings.cpp`, `test_processview.cpp`, `test_processrunner.cpp` | Real process-tree exit proof, cleanup after client exit and preserved ignored/unknown/local files; no unknown-result replay |
| Swarm/mobile | `test_swarmcontroller.cpp`, `test_workspace.cpp`, `test_workspacesettings.cpp`, `test_relaysettings.cpp`, `test_windowsmobilesettings.cpp`, `test_mobiledeliverysettings.cpp`, `test_phonepairing.cpp`, `test_desktoppresence.cpp` | No-GUI catalog/worker and presence behavior, encrypted connector migration without credential disclosure or service interruption |
| Terminal/external programs | `test_terminalbackend.cpp`, `test_terminalview.cpp`, `test_externalfile.cpp` | Latest supported native agents on Linux/macOS/Windows, ConPTY/IME/paste/resize and detach survival; PowerShell/argv/file-origin safeguards |
| Native platform/tray | `test_snitrayicon.cpp`, `test_traymenu.cpp`, `test_quiettrayicon.cpp`, `test_linuxattention.cpp`, `test_linuxapplicationbadge.cpp`, `test_linuxpresence.cpp`, `test_kwinwindowlayer.cpp`, `test_macapplicationbadge.mm`, `test_macappvisibility.cpp`, `test_macattention.mm`, `test_macbackend.cpp`, `test_macpresence.cpp`, `test_qttrayicon.mm`, `test_windowsdesktop.cpp`, `test_appconfig.cpp` | Real shell/menu/notification/activation/single-instance behavior on all three platforms; current offscreen suites do not prove these native paths |
| Updates/startup/watchdog | `test_updates.cpp`, `test_terminationsignals.cpp`, `test_watchdog.cpp`, `test_ui_language.cpp` | Installer/upgrade/rollback with running native agents, compatible privacy identity, update-feed validation and no unexpected restart |
| Voice/video/native browser | `test_speech.cpp`, `test_macos_microphone_permissions.mm`, `test_nativeuiview.cpp`, `test_nativeuiplugin.cpp` | Real consented microphone/camera, private recordings, provider-status recovery, no duplicate audio and no Qt WebEngine/plugin remnants |
| Shared rendering/cache | `test_iconfactory.cpp`, `test_paintcache.cpp`, `test_archivebrowser.cpp`, `test_downloadcache.cpp`, `test_downloadcachesettings.cpp`, `test_eventstream.cpp`, `test_jsonparse.cpp`, `test_analytics.cpp`, `test_automaticapprovals.cpp` | Native visual/performance acceptance plus durable fanout/cache maintenance and migration of malformed/unknown records |

The final removal gate must also run the source and distribution checks named in `CONTRIBUTING.md`, the platform package checks, real native-agent contract tests, migration-upgrade tests and a file/dependency audit. No claim of Qt retirement is valid while a responsibility is still mapped only to a Qt process, an unported Qt-only store, or an unverified native integration.
