import { describe, expect, it } from "vitest";
import { APPROVALS_TITLE, APPROVALS_BODY, APPROVALS_CONFIRM, APPROVALS_WARNING, APPROVALS_HINT } from "./approval-opt-in";

// Every text Settings shows for a Qt control is Qt's own: it appears in the
// Qt source it comes from and in the section that shows it.

// Read from disk, as styles/motion.test.ts does: the specifier is built so
// TypeScript (no Node types here) leaves it alone.
const fs = (await import(/* @vite-ignore */ ["node", "fs"].join(":"))) as {
  readFileSync(path: URL, encoding: "utf8"): string;
};
const here = new URL("./", import.meta.url);
const repo = new URL("../../../../../", import.meta.url);
const qt = ["tray/src/SettingsPage.h", "tray/src/UiLanguage.h"].map((file) => fs.readFileSync(new URL(file, repo), "utf8")).join("\n");
const source = (file: string) => fs.readFileSync(new URL(file, here), "utf8");
const layer = fs.readFileSync(new URL("tray/src/WindowLayer.cpp", repo), "utf8");
// Since the shared Keep Above state (zerus-5wmh.24.2) the hints come from the
// window layer: its store and the native layer that reports them.
const clientLayer = [fs.readFileSync(new URL("../../../store/windowLayer.ts", here), "utf8"),
  fs.readFileSync(new URL("client/src-tauri/src/window_layer.rs", repo), "utf8")].join("\n");
const keyboard = fs.readFileSync(new URL("tray/src/KeyboardSettings.cpp", repo), "utf8");
const shortcuts = fs.readFileSync(new URL("tray/src/Shortcuts.cpp", repo), "utf8");
const store = fs.readFileSync(new URL("../../../store/shortcuts.ts", here), "utf8");
const speech = fs.readFileSync(new URL("tray/src/speech/SpeechSettings.cpp", repo), "utf8");
const speechCore = fs.readFileSync(new URL("client/core/src/speech_settings.rs", repo), "utf8");

const APPEARANCE = [
  "Follow system appearance", "Dark", "Light", "Interface language",
  "Restart Zerus to apply the interface language. Agent sessions keep running.",
  "Russian is a preview. Untranslated text stays in English.", "Russian (preview)",
  "Usage limit indicator", "Usage and elapsed time bars", "Pace marker", "Pace marker and reset countdown",
  "Compares used quota with the elapsed part of each limit window. The bars show usage above elapsed time; the marker shows elapsed time on the usage bar. Colors warn when the current pace would use up the limit before it resets.",
  "Content scale",
  "Adjusts Activity, the message field and Terminal from 75% to 200%. The session list and the side panel keep their size.",
  "Appearance and session list preferences apply to this Zerus.",
  "Activity width", "Maximum Activity width", "Full-width Activity", "Keep Zerus above other windows",
];
// Qt texts with an argument, as Appearance fills it in.
const APPEARANCE_FILLED: [string, string][] = [
  ["Reset to %1 px", "Reset to 900 px"],
  ["In a wide window, Activity, questions and the message field stay centered at this width, which grows with content scale. Drag the column edges in Activity to adjust it, or double-click an edge to restore %1 px. Full-width Activity fills the whole pane.",
    "In a wide window, Activity, questions and the message field stay centered at this width, which grows with content scale. Drag the column edges in Activity to adjust it, or double-click an edge to restore 900 px. Full-width Activity fills the whole pane."],
];
const WINDOW_LAYER = [
  "Files and terminals opened from Zerus may appear behind it while this is on.",
  "Keep Above is unavailable in this Zerus session. On Wayland, KDE Plasma 6 with KWin scripting is required. You can use your desktop's window menu or rules.",
];
const SESSIONS = [
  "Expand the collapsed session list on hover", "Hide projects without active sessions", "Animate working sessions",
  "Show completed subagents",
  "Include agents whose latest turn has finished in the session list and its counts. Plan & agents always shows all reported agents. Agents with unknown status remain visible.",
  "Notify about optional questions",
  "Off by default. Shows a system notification once for each new optional question, which the agent keeps working without. It is withdrawn when the questions are answered or skipped, or the session is marked as read. Questions that need an answer always notify.",
  "Closing Zerus keeps agents running. Projects and the arrangement of your workspace are saved on this device.",
];

const KEYBOARD = [
  "Keyboard", "Command", "Shortcut", "Alternate", "Reset", "Reset all", "Restore the default chords of the selected command",
  "Chords marked ✓ work in Terminal.", "Double-click a shortcut, or press Enter on it, to change it.", "Works in Terminal",
  "Reset all shortcuts", "Restore the default chords of every command?", "Change shortcut", "Clear", "Shortcut in use",
  "Press the new shortcut for %1.", "Already used by %1. Reassign?",
];
const ABOUT_FILLED: [string, string][] = [
  ["Zerus %1", "`Zerus ${__APP_VERSION__}`"],
  ["hgs CLI %1", "`hgs CLI ${version}`"],
];
const ABOUT = ["About", "hgs CLI version unavailable", "hgs CLI: checking…", "Open Zerus repository on GitHub"];

const VOICE = [
  "Voice input", "Preview before sending",
  "Show Stop instead of Send while recording audio or video. Stop retains a preview for review and a separate Send. This preference applies on this device.",
  "Manage recognition providers on a processing computer. Recording starts on this device; Send voice uses the provider selected beside your recording.",
  "Configure providers on", "Add", "Edit", "Test", "Remove", "Use as default",
  "Test sends a short synthetic recording from the selected computer. It may load a prepared model or use the remote API; it does not use your microphone or send anything to an agent.",
  "Speech provider documentation", "Refresh providers and microphones", "Check operation status", "Microphone on this device",
  "System default microphone", "Selected microphone (unavailable)",
  "Provider settings and credentials stay on their processing computer. Recordings are limited to two minutes. Original audio and recognized text remain in conversation history; your typed draft stays separate.",
  "Remove speech provider?", "Remove provider", "Name", "Profile ID", "Type", "Model", "Language", "Device", "Base URL",
  "Allow unencrypted HTTP for a local or private server", "API key", "Keep stored API key", "No API key", "Replace API key",
  "Remove API key", "New API key", "Auto-detect", "e.g. Team transcription server", "e.g. Russian speech",
  "Unsupported provider type", "HTTP transcription documentation", "Local GigaAM documentation",
  "This provider type cannot be edited by this version of Zerus. See the provider documentation before changing it.",
  String.raw`The processing computer sends audio to the server's /audio/transcriptions endpoint. Use the server's model ID and a two-letter language code, or leave language empty.\n\nThe optional API key is saved on the processing computer. Prefer HTTPS; private HTTP requires explicit consent. Saving sends no audio.`,
  String.raw`Local inference runs on the processing computer named above. Saving creates a profile; it does not install the service or download model weights.\n\nv3_e2e_rnnt is the default for Russian with punctuation; use ru for Russian.\n\nCPU uses the processor. CUDA requires a supported NVIDIA GPU and CUDA-enabled PyTorch. MPS requires a supported Apple GPU/runtime. Use Test to verify the chosen model and device.`,
];
// Texts the core's state machine (speech_settings.rs) shows.
const VOICE_CORE = [
  "Checking provider configuration…", "Loading this computer's voice statistics…", "Voice statistics unavailable.",
  "The computer returned an invalid provider configuration. Update hgs and refresh.",
  "The recognition service is not ready. You can still add and edit providers.",
  "No providers yet. Add Local GigaAM or an OpenAI-compatible server.",
  "Provider configuration is ready. Test a saved provider before using it.",
  "Use as default selects a provider for new recordings. Existing recordings keep their selected provider.",
  "The selected default provider is unavailable. Choose another explicitly or restore it on this computer.",
  "The default provider is configured on another computer. Browsing these providers does not change it.",
  "Working on the provider request. You can keep using Zerus.",
  "That profile ID already exists. Edit the existing provider or choose another ID.",
  "Saving on the selected computer. If the result is not confirmed, use Check operation status.",
  "The operation is not confirmed. Check its status before making another change.",
  "The saved configuration changed. Your edits are kept here. Close and reopen the provider to review its current values before saving.",
  "Could not save the operation identity. No provider request was sent.", "The provider request failed.",
  "Prepare the speech service on this processing computer before testing.", "HTTP server", "Local GigaAM", "Ready", "Not ready",
  "Add speech provider", "Edit speech provider", "This Mac", "This Windows computer", "This computer (Linux)",
];
const VOICE_CORE_FILLED: [string, string][] = [
  ["Default for new recordings: %1", "Default for new recordings: {}"],
  ["Processing computer: %1", "Processing computer: {computer}"],
  ["The provider test on %1 completed successfully.", "The provider test on {computer} completed successfully."],
  ["Provider configuration saved on %1.", "Provider configuration saved on {computer}."],
  ["A provider request on %1 is not yet confirmed. Check its status; it will not be sent again automatically.",
    "A provider request on {} is not yet confirmed. Check its status; it will not be sent again automatically."],
  ["Recognition service is available on %1. Test the saved profile to verify the selected provider.",
    "Recognition service is available on {computer}. Test the saved profile to verify the selected provider."],
  ["Recognition service is not ready on %1. %2", "Recognition service is not ready on {computer}. {}"],
];

const recoveryQt = fs.readFileSync(new URL("tray/src/RecoverySettings.h", repo), "utf8");
const recoveryCore = fs.readFileSync(new URL("client/core/src/recovery_settings.rs", repo), "utf8");
const RECOVERY = [
  "Automatic recovery", "Shared by all connected machines. Agents finish their own retries first; recovery continues the same task.",
  "Recover automatically after temporary failures", "Overload / server unavailable", "Temporary rate limit", "Session usage limit",
  "Connection / timeout", "Wait for reset", "Retry on a schedule",
  "Wait until the provider's reset time by default. If no reset time is available, recovery needs your attention. Choose a schedule to retry at your own intervals.",
  "Retry after (seconds)", "Save recovery settings", "Reload", "Saved.",
  String.raw`Each positive number is a delay before the next attempt. End with −1 to stop, or 0 to repeat the previous delay forever.\nExample: 300, 0 retries every 5 minutes until stopped.\n\nNew messages and Stop cancel recovery. Sign-in, balance, other quota and context errors require your action.`,
];
const RECOVERY_CORE = [
  "To configure automatic recovery, enable local sessions for this computer in Machines or add a remote machine.",
  "Add a remote machine in Machines to configure automatic recovery; this computer runs no agents.",
  "Synchronizing recovery settings…", "Synchronized with 1 machine. Changes from any Zerus are shared.",
  "Wait for synchronization to finish before saving.", "Settings changed in another Zerus. Reload before saving.",
  "Connect a remote machine to save recovery settings.", "Update HGS on this machine to synchronize recovery settings.",
  "this machine", "Use 1–16 values: delays from 1 to 86400 seconds; −1 or 0 may only be last, and 0 needs a preceding delay.",
];

const filesQt = fs.readFileSync(new URL("tray/src/DownloadCacheSettings.cpp", repo), "utf8");
const cacheQt = fs.readFileSync(new URL("tray/src/DownloadCache.cpp", repo), "utf8");
const cacheCore = fs.readFileSync(new URL("client/core/src/download_cache.rs", repo), "utf8");
const FILES = [
  "Files",
  "Download and open reuses a verified saved copy from the same machine and path. Show file selects it in your file manager. Download again gets a fresh copy; Download… lets you choose where to save. Existing files are preserved.",
  "Downloaded files", "Never clear", "Limit size", "Keep copies", "Maximum size",
  "Never clear is the default. With a limit, Zerus removes its least recently used downloaded copies first. Files you add or edit are kept. Downloads in progress and copies used in the last minute are protected, so usage can briefly exceed the limit. This setting applies to this computer.",
  "Checking downloaded files…", "Apply", "Limit downloaded files?", "Apply limit",
];
const FILES_CORE = [
  "Cannot create the downloaded files folder.", "The downloaded files folder is unavailable or is a link.",
  "The downloaded files cache is busy.", "Cannot read the downloaded files index.",
  "The downloaded files index is invalid. Existing files were preserved.", "Cannot save the downloaded files index.",
  "Some older downloaded copies could not be removed.", "The downloaded files index is a link.",
  "Cannot prepare the local download.", "The downloaded file source is invalid.", "The prepared download is unavailable.",
  "The downloaded copy could not be verified.", "Cannot save the downloaded copy in its cache folder.",
  "Cannot find an unused name for the downloaded copy.",
];

const analyticsQt = ["tray/src/AnalyticsSettings.cpp", "tray/src/AnalyticsClient.cpp"].map((file) => fs.readFileSync(new URL(file, repo), "utf8")).join("\n");
const analyticsCore = ["client/core/src/analytics.rs", "client/core/src/analytics/page.rs", "client/src-tauri/src/analytics.rs"]
  .map((file) => fs.readFileSync(new URL(file, repo), "utf8")).join("\n");
const ANALYTICS = [
  "Private analytics",
  "Content-free daily counters stay on each computer. Reads do not enable collection. Unknown measures are unavailable, not zero. Dates are inclusive UTC days (up to 90).",
  "Selected computers", "This computer", "All configured computers", "First 32 configured computers", "Apply and refresh",
  "From", "Through", "From UTC day", "Through UTC day", "Choose a range and refresh to read analytics.",
  "Daily agent work", "Provider", "All providers", "UTC day", "Computer", "Producer", "Completed turns", "Work",
  "Approval wait", "Input wait", "Previous", "Next", " (last known)", "Reading…",
  "Select a day to see all reported counts, token usage, typed failures and histogram means.",
  "Voice on processing computers",
  "Voice counts are separate from agent turns. Accepted audio and total queue/processing time have units; percentile latency is unavailable. Speech collection is managed independently on its processing host.",
  "Collection on this computer", "Collect content-free agent analytics", "Daily aggregate retention", "Apply locally",
  "Purge local history…",
  "Disabling collection keeps existing history. Purge removes only this computer's agent analytics, not speech statistics or durable delivery receipts. Remote collection controls are read-only here.",
  "Review aggregate diagnostics…", "Shorten local analytics history?", "Shorten retention", "Purge local agent analytics?",
  "Permanently delete all retained agent analytics on this computer. Speech statistics, other computers and durable delivery receipts are unaffected. Collection keeps its current enabled/disabled setting.",
  "Purge local history", "Review aggregate diagnostics",
  "Review these aggregate fields before saving a local JSON file. Computer numbers are anonymous within this file. This export includes no logs, message content, account details or native identifiers, and is never uploaded automatically.",
  "Save file…", "This view includes at most 32 configured computers. Additional configured routes are omitted.",
];
const ANALYTICS_CORE = [
  "Reading…", "Read finished. Last-known or missing data is excluded from current totals.",
  "Retired, older or ambiguous analytics source. Excluded from current totals.",
  "Summary is stale or its clock is unconfirmed.", "No current read available.", "Applying local collection settings…",
  "Local analytics epoch is stale or ambiguous. Refresh before purging.", "Choose an inclusive UTC range of at most 90 days.",
  "Computer unavailable. No current analytics read.", "This computer does not support the requested analytics read.",
  "Change was not confirmed. Refresh and review before another action.",
  "Analytics read failed or exceeded its response limit.", "Computer returned invalid analytics data.",
  "Aggregate diagnostics saved locally.", "Could not save aggregate diagnostics.",
];

const approvalsQt = ["tray/src/AutomaticApprovalSettings.cpp", "tray/src/HgsClient.cpp", "tray/src/SettingsPage.h"]
  .map((file) => fs.readFileSync(new URL(file, repo), "utf8")).join("\n");
const approvalsCore = fs.readFileSync(new URL("client/core/src/approvals.rs", repo), "utf8");
const APPROVALS = [
  "Automatic approvals", "Automatically approve tool requests (dangerous)", "Approval journal",
  "Exact computer name (optional)", "Exact session name (optional)", "Refresh", "Time", "Computer", "Session", "Question",
  "Result", "Newer", "Older", "Up to 50 entries per page", "Select a journal entry to read the question and its outcome.",
  "Loading question…", "Loading approval journal…", "No automatic approvals recorded for this filter.",
];
const APPROVALS_CORE = [
  "Approved once", "Failed", "Cancelled", "Unconfirmed", "Invalid approval journal page.",
  "Invalid automatic approval journal response.",
  "The automatic approval journal is unavailable. No new approval was authorized.",
  "Update the local hgs CLI to use automatic approvals and its journal.",
];

const worktreesQt = fs.readFileSync(new URL("tray/src/WorktreeSettings.h", repo), "utf8");
const worktreesCore = ["client/core/src/worktree_policy.rs", "client/core/src/worktree_policy/page.rs"]
  .map((path) => fs.readFileSync(new URL(path, repo), "utf8")).join("\n");
const WORKTREES = [
  "Worktrees", "This computer", "Refresh",
  "Cleanup runs on the computer that owns each worktree, even when this window is closed. Rules are enabled by default and apply across its projects. Other computers keep their own settings.",
  "Enable automatic worktree cleanup", "Clean integrated worktrees after the last session is archived or terminated",
  "Periodically clean unused integrated worktrees", "Forget verified managed worktrees whose folders no longer exist",
  "Minimum inactivity for periodic cleanup",
  "Only verified Zerus-created worktrees qualify. Cleanup keeps branches and conversation history. Changed, untracked and ignored local files, sessions, running programs, locked folders and incomplete checks protect a worktree. Build folders and local configuration require review in Projects > Worktrees.",
  "Refresh to read this computer's cleanup rules.", "Apply to this computer",
];
const WORKTREES_CORE = [
  "Refresh to read this computer's cleanup rules.", "Reading cleanup settings…",
  "Cleanup settings saved on the selected computer.", "Cleanup settings loaded. Uncertain removals stay for manual review.",
  "This computer is unavailable or needs an updated hgs CLI for cleanup settings.",
  "Save was not confirmed. Refresh before changing settings again.", "Could not read cleanup settings.",
  "Computer returned invalid cleanup settings.", "worktree-maintenance-v1",
];

const mobileQt = ["tray/src/WorkspaceSettings.h", "tray/src/PhonePairing.cpp", "tray/src/MobileDeliverySettings.h", "tray/src/SettingsPage.h", "tray/src/HgsClient.cpp"]
  .map((file) => fs.readFileSync(new URL(file, repo), "utf8")).join("\n");
const mobileCore = ["client/core/src/mobile.rs", "client/core/src/mobile/page.rs"]
  .map((path) => fs.readFileSync(new URL(path, repo), "utf8")).join("\n");
const MOBILE = [
  "Mobile connection", "Workspace",
  "Connect your computers and phones to one workspace. Session content is encrypted between your devices; the relay keeps routing and delivery metadata.",
  "Workspace: checking…", "Workspace status is unconfirmed", "Change workspace…", "Start connector", "Stop connector",
  "Refresh status", "Give computer code", "Copy computer code", "Cancel code", "Create workspace",
  "Advanced: legacy connector configuration",
  "Phones",
  "Phone", "Platform", "Paired", "Last active", "Added from", "Pair a phone…", "Revoke…", "Refresh", "Checking phones…",
  "Verified device membership. Private device names are not stored by the relay.",
  "Paired with an administrator invitation, or before Zerus recorded it",
  "Phone notifications", "When I'm not at a computer, send notifications to my phone",
  "Immediately, even if I'm at a computer", "As soon as I'm away", "After 5 minutes away", "After 10 minutes away",
  "After 15 minutes away", "After 30 minutes away",
  "You count as away after two minutes without keyboard or mouse input on every computer running Zerus, or while their screens are locked. Your computers and the phone share this choice.",
];
const MOBILE_CORE = [
  "Private session content is encrypted.", "Connector: running", "Connector: draining its current request", "Connector: stopped",
  "This computer was removed from its workspace.",
  "The retired connector is draining. You can stop its current generation before changing workspace.",
  "Choose Change workspace to create a workspace or join with a new invitation. Saved local content stays on this computer.",
  "Legacy connection: session content is visible to this relay.", "Choose Change workspace to create or join an encrypted workspace.",
  "Workspace activation is incomplete.", "Check the saved connection before continuing.", "No workspace connected",
  "Create a workspace or use a computer invitation from an enrolled computer.",
  "The new encrypted workspace is ready. The old relay has not confirmed retirement. You can explicitly disconnect its local configuration before switching.",
  "A workspace connection is saved and waiting. Check its result before starting another connection.",
  "Workspace operation was not confirmed. Refresh status before continuing. Verify that the matching computer connector is installed.",
  "Open Settings → Mobile connection to create a workspace or join one by code, then start the connector and refresh Phones.",
  "No computer here has a mobile connection. Set one up in Settings → Mobile connection on a computer with local sessions enabled, then refresh Phones.",
  "1 phone", "this computer", "Invalid phone management response", "mobile phones|invite|revoke-phone",
  "Check connection", "Continue connecting", "Disconnect legacy locally and switch…",
  "Saving…", "Saved. Your other computers and the phone receive it with the next sync.",
  "launchctl kickstart -k gui/$(id -u)/com.hgdev.hgs-swarm", "systemctl --user restart hgs-swarm.service",
];

const PROCESSES = [
  "Processes", "Enable Processes tab", "Open Processes tab", "Background process count", "Unconfirmed process output", "Show loading indicator after",
  "Off by default. When disabled, the tab is hidden and processes are not polled. Enable it to inspect running shells and background commands, read recorded output and stop selected processes. Only the selected session is inspected; opening Processes refreshes immediately. Output is read only while the tab is visible. Timings apply to this Zerus. Background refresh also controls session inspection while Terminal or Native UI is open; session overview and account limits refresh separately.",
];
// The Processes tab and its session menu entry (ProcessView.cpp, SessionsWindow.cpp).
const processView = fs.readFileSync(new URL("tray/src/ProcessView.cpp", repo), "utf8");
const sessionsWindow = fs.readFileSync(new URL("tray/src/SessionsWindow.cpp", repo), "utf8");
const sessionsUi = (file: string) => fs.readFileSync(new URL(`../../sessions/${file}`, here), "utf8");
const processStore = fs.readFileSync(new URL("../../../store/processes.ts", here), "utf8");
const PROCESS_TAB = ["Command", "Agent", "State", "Duration", "Terminate", "Copy command", "Select a command to view its output."];
const PROCESS_MENU = ["Show running commands", "Follow this session's background work in Processes", "Enable Settings > Processes to inspect running commands"];

describe("Settings texts", () => {
  it.each(PROCESSES)("Processes shows Qt's %s", (text) => {
    expect(qt).toContain(`tr("${text}")`);
    expect(source("Processes.tsx")).toContain(text);
  });
  it("Processes keeps Qt's timing ranges", () => {
    const TIMINGS = [...source("Processes.tsx").matchAll(/\{ key: "([^"]+)", label: (?:trNoop\()?"([^"]+)"\)?, min: ([\d.]+), max: ([\d.]+) \}/g)]
      .map((m) => ({ key: m[1], label: m[2], min: Number(m[3]), max: Number(m[4]) }));
    const qtTimings = [...qt.matchAll(/timing\("(\w+)",tr\("([^"]+)"\),ProcessSettings::\w+\(\),([\d.]+),([\d.]+)\)/g)]
      .map((m) => ({ key: `processes/${m[1]}`, label: m[2], min: Number(m[3]), max: Number(m[4]) }));
    expect(qtTimings.length).toBe(4);
    expect(TIMINGS.map((t) => ({ key: t.key, label: t.label, min: t.min, max: t.max }))).toEqual(qtTimings);
  });
  it.each(PROCESS_TAB)("the Processes tab shows Qt's %s", (text) => {
    expect(processView).toContain(`tr("${text}`);
    expect(sessionsUi("Processes.tsx")).toContain(text);
  });
  it("the Processes tab fills in Qt's counts", () => {
    expect(processView).toContain('tr("Terminate selected (%1)")');
    expect(sessionsUi("Processes.tsx")).toContain(`tr("Terminate selected (%1)", ids.length)`);
    expect(sessionsWindow).toContain('tr("Processes (%1)")');
    expect(processStore).toContain('tr("Processes (%1)", count)');
  });
  it.each(PROCESS_MENU)("the session menu shows Qt's %s", (text) => {
    expect(sessionsWindow).toContain(`tr("${text}")`);
    expect(processStore).toContain(text);
  });

  it.each(ANALYTICS)("Private analytics shows Qt's %s", (text) => {
    expect(analyticsQt).toContain(`"${text}"`);
    expect(source("Analytics.tsx") + source("analytics-text.ts")).toContain(text);
  });
  it.each(ANALYTICS_CORE)("the analytics core says Qt's %s", (text) => {
    expect(analyticsQt).toContain(`"${text}"`);
    expect(analyticsCore).toContain(`"${text}"`);
  });
  it("the analytics core counts the routes it reads as Qt does", () => {
    expect(analyticsQt).toContain('"Reading %1 configured route(s)…"');
    expect(analyticsCore).toContain('"Reading {} configured route(s)…"');
  });
  it.each([APPROVALS_TITLE, APPROVALS_BODY, APPROVALS_CONFIRM, APPROVALS_WARNING, APPROVALS_HINT])("the dangerous opt-in uses Qt and the mockup's text: %s", (text) => {
    const mockup = fs.readFileSync(new URL("design/webui/src/screens/settings/Policies.tsx", repo), "utf8") + fs.readFileSync(new URL("design/webui/src/dialogs/SettingsDialogs.tsx", repo), "utf8");
    expect(approvalsQt).toContain(text); expect(mockup).toContain(text);
  });
  it.each(APPROVALS)("Automatic approvals shows Qt's %s", (text) => {
    expect(approvalsQt).toContain(`"${text}"`);
    expect(source("Approvals.tsx") + source("approvals-journal.ts")).toContain(text);
  });
  it.each(APPROVALS_CORE)("the approval journal says Qt's %s", (text) => {
    expect(approvalsQt).toContain(`"${text}"`);
    expect(approvalsCore).toContain(`"${text}"`);
  });
  it("the approval journal fills in Qt's counts and answers", () => {
    expect(approvalsQt).toContain('"%1 entries. Unconfirmed attempts require checking Activity or Terminal; they are not retried."');
    expect(source("approvals-journal.ts")).toContain("${page.entries.length} entries. Unconfirmed attempts require checking Activity or Terminal; they are not retried.");
    expect(approvalsQt).toContain('"Selected answer: %1"');
    expect(approvalsCore).toContain('"Selected answer: {}"');
  });
  it.each(WORKTREES)("Worktrees shows Qt's %s", (text) => {
    expect(worktreesQt).toContain(`"${text}"`);
    expect(source("Worktrees.tsx")).toContain(text);
  });
  it.each(WORKTREES_CORE)("the cleanup policy says Qt's %s", (text) => {
    expect(worktreesQt).toContain(`"${text}"`);
    expect(worktreesCore).toContain(`"${text}"`);
  });
  it.each(MOBILE)("Mobile connection shows Qt's %s", (text) => {
    expect(mobileQt).toContain(`"${text}"`);
    expect(source("Mobile.tsx") + source("ComputerInvitations.tsx") + source("WorkspacePanel.tsx")).toContain(text);
  });
  it.each(MOBILE_CORE)("the mobile core says Qt's %s", (text) => {
    expect(mobileQt.replaceAll("\\n", "\n")).toContain(text);
    expect(mobileCore).toContain(text);
  });
  it("Mobile connection shows Qt's Phones introduction, which Qt writes in two pieces", () => {
    expect(mobileQt).toContain('"Phones paired with your computers. A phone can control every computer of its workspace; "\n                                "revoke one you no longer use."');
    expect(source("Mobile.tsx")).toContain("Phones paired with your computers. A phone can control every computer of its workspace; revoke one you no longer use.");
  });
  it("the mobile core fills in Qt's formats", () => {
    expect(mobileQt).toContain('"Update Zerus on %1 to manage phones from here."');
    expect(mobileCore).toContain('"Update Zerus on {computer} to manage phones from here."');
    expect(mobileQt).toContain('"Could not read the machine list: %1"');
    expect(mobileCore).toContain('"Could not read the machine list: {error}"');
    expect(mobileQt).toContain('"%1 phones"');
    expect(mobileCore).toContain('"{count} phones"');
    expect(mobileQt).toContain('"%1 (via %2)"');
    expect(source("Mobile.tsx")).toContain("(via ${list.host})");
    expect(mobileQt).toContain('"Could not save: %1"');
    expect(mobileCore).toContain('"Could not save: {}"');
    expect(mobileQt).toContain('"Could not read the shared setting: %1"');
    expect(mobileCore).toContain('"Could not read the shared setting: {}"');
    expect(mobileQt).toContain('"Update Zerus on %1, a computer this one synchronizes with directly. Until then it keeps syncing projects but does not receive this choice."');
    expect(mobileCore).toContain('"Update Zerus on {one}, a computer this one synchronizes with directly. Until then it keeps syncing projects but does not receive this choice."');
    expect(mobileQt).toContain('"Update Zerus on these computers this one synchronizes with directly: %1. Until then they keep syncing projects but do not receive this choice."');
    expect(mobileCore).toContain('"Update Zerus on these computers this one synchronizes with directly: {}. Until then they keep syncing projects but do not receive this choice."');
    expect(mobileQt).toContain('"The sync service on this computer is outdated and must be restarted (%1, or sign out and back in). Until then this computer may stop syncing projects and this choice."');
    expect(mobileCore).toContain('"The sync service on this computer is outdated and must be restarted ({}, or sign out and back in). Until then this computer may stop syncing projects and this choice."');
  });
  it.each(FILES)("Files shows Qt's %s", (text) => {
    expect(filesQt).toContain(`"${text}"`);
    expect(source("Files.tsx")).toContain(text);
  });
  it("Files fills in Qt's limit question", () => {
    expect(filesQt).toContain('"Zerus will automatically delete its oldest downloaded copies when they exceed %1. Files you added or edited will be kept."');
    expect(source("Files.tsx")).toContain('tr("Zerus will automatically delete its oldest downloaded copies when they exceed %1. Files you added or edited will be kept.", formatDataSize(props.bytes))');
  });
  it.each(FILES_CORE)("the download cache says Qt's %s", (text) => {
    expect(cacheQt).toContain(`"${text}"`);
    expect(cacheCore).toContain(`"${text}"`);
  });
  it.each(RECOVERY)("Automatic recovery shows Qt's %s", (text) => {
    expect(recoveryQt).toContain(`"${text}"`);
    expect(source("Recovery.tsx")).toContain(text);
  });
  it.each(RECOVERY_CORE)("the recovery core says Qt's %s", (text) => {
    expect(recoveryQt).toContain(`"${text}"`);
    expect(recoveryCore).toContain(`"${text}"`);
  });
  it.each(VOICE)("Voice input shows Qt's %s", (text) => {
    expect(speech).toContain(`"${text}"`);
    expect(source("Voice.tsx")).toContain(text);
  });
  it.each(VOICE_CORE)("the Voice input core says Qt's %s", (text) => {
    expect(speech).toContain(`"${text}"`);
    expect(speechCore).toContain(`"${text}"`);
  });
  it.each(VOICE_CORE_FILLED)("the Voice input core fills in Qt's %s", (template, shown) => {
    expect(speech).toContain(`"${template}"`);
    expect(speechCore).toContain(shown);
  });
  it.each(ABOUT)("About shows Qt's %s", (text) => {
    expect(qt).toContain(`"${text}"`);
    expect(source("About.tsx")).toContain(text);
  });
  it.each(ABOUT_FILLED)("About fills in Qt's %s", (template, shown) => {
    expect(qt).toContain(`"${template}"`);
    expect(source("About.tsx")).toContain(shown);
  });
  it("About names the build as UpdateController::installedVersion does", () => {
    const updates = fs.readFileSync(new URL("tray/src/UpdateController.cpp", repo), "utf8");
    expect(updates).toContain('tr("%1 (local changes %2)")');
    expect(source("about-version.ts")).toContain('tr("%1 (local changes %2)", version, source)');
    expect(updates).toContain('QString("%1 (%2)")');
    expect(source("about-version.ts")).toContain("`${version} (${source})`");
  });
  it.each(KEYBOARD)("Keyboard shows Qt's %s", (text) => {
    expect(keyboard).toContain(`"${text}"`);
    expect(source("Keyboard.tsx")).toContain(text);
  });
  it("uses Shortcuts.cpp's titles, groups, reasons and Terminal rules", () => {
    const qtTexts = [...shortcuts.matchAll(/(?:QT_TRANSLATE_NOOP\("Shortcuts", |translate\("Shortcuts", )"((?:[^"\\]|\\.)+)"/g)].map((m) => m[1]);
    expect(qtTexts.length).toBeGreaterThan(40);
    for (const text of qtTexts) expect(store, text).toContain(`"${text}"`);
  });
  it.each(APPEARANCE)("Appearance shows Qt's %s", (text) => {
    expect(qt).toContain(`"${text}"`);
    expect(source("Appearance.tsx")).toContain(text);
  });
  it.each(APPEARANCE_FILLED)("Appearance fills in Qt's %s", (template, shown) => {
    expect(qt).toContain(`"${template}"`);
    expect(source("Appearance.tsx")).toContain(shown);
  });
  it.each(WINDOW_LAYER)("Keep Above shows WindowLayer's %s", (text) => {
    expect(layer).toContain(`"${text}"`);
    expect(clientLayer).toContain(text);
  });
  it.each(SESSIONS)("Sessions shows Qt's %s", (text) => {
    expect(qt).toContain(`"${text}"`);
    expect(source("Sessions.tsx")).toContain(text);
  });
  it("lists the sections in Qt's order", () => {
    const labels = [...source("sections.ts").matchAll(/label: (?:trNoop\()?"([^"]+)"/g)].map((m) => m[1]);
    const nav = /nav->addItems\(\{([^}]*)\}\)/.exec(qt)![1];
    const qtOrder = [...nav.matchAll(/tr\("([^"]+)"\)/g)].map((m) => m[1]);
    // Worktrees goes in before Automatic approvals (SettingsPage.h:140).
    qtOrder.splice(qtOrder.indexOf("Automatic approvals"), 0, "Worktrees");
    // WhiteWh fork: Extensions (SettingsPage.h) is in the Qt desktop only.
    const fork = qtOrder.indexOf("Extensions");
    if (fork >= 0) qtOrder.splice(fork, 1);
    expect(labels).toEqual(qtOrder);
  });
});
