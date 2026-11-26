import { createSignal, type JSX } from "solid-js";
import { setPage } from "../../../store/ui";
import { AppearanceSection } from "./Appearance";
import { SessionsSection } from "./Sessions";
import { TeamsSection } from "./TeamsSection";
import { MemorySection } from "./MemorySection";
import { SharedHostsSection } from "./SharedHostsSection";
import { ModelServersSection } from "./ModelServersSection";
import { KeyboardSection } from "./Keyboard";
import { VoiceSection, refreshVoice } from "./Voice";
import { RecoverySection, refreshRecovery } from "./Recovery";
import { FilesSection } from "./Files";
import { AnalyticsSection } from "./Analytics";
import { ApprovalsSection } from "./Approvals";
import { ExtensionsSection } from "./Extensions";
import { WorktreesSection, refreshWorktrees } from "./Worktrees";
import { DiskSection } from "./Disk";
import { MobileSection } from "./Mobile";
import { ProcessesSection } from "./Processes";
import { AboutSection, refreshAbout } from "./About";
import { UpdatesSection } from "./Updates";
import { refreshUpdates } from "../../../store/updates";
import { trNoop } from "../../../i18n";

// Settings sections in Qt's order (tray/src/SettingsPage.h:43, Worktrees
// inserted at :140). A section shows once it has a component; `refresh` runs
// whenever it is opened (SettingsPage.h:157). Phase 4 Task 11 adds Processes
// here as ProcessesSection (./Processes.tsx).

export type SectionId =
  | "appearance" | "sessions" | "teams" | "recovery" | "processes" | "updates" | "mobile" | "files"
  | "worktrees" | "disk" | "approvals" | "voice" | "keyboard" | "analytics" | "extensions" | "about" | "modelservers"
  | "memory" | "sharedhosts";

export interface Section {
  id: SectionId;
  label: string;
  icon: string;
  component?: () => JSX.Element;
  refresh?: () => void;
}

export const SECTIONS: readonly Section[] = [
  { id: "appearance", label: "Appearance", icon: "palette", component: AppearanceSection },
  { id: "sessions", label: "Sessions", icon: "rows-3", component: SessionsSection },
  { id: "recovery", label: trNoop("Automatic recovery"), icon: "rotate-ccw", component: RecoverySection, refresh: refreshRecovery },
  { id: "processes", label: "Processes", icon: "activity", component: ProcessesSection },
  { id: "updates", label: "Updates", icon: "download", component: UpdatesSection, refresh: refreshUpdates },
  { id: "mobile", label: trNoop("Mobile connection"), icon: "smartphone", component: MobileSection },
  { id: "files", label: "Files", icon: "hard-drive", component: FilesSection },
  { id: "worktrees", label: "Worktrees", icon: "folder-git-2", component: WorktreesSection, refresh: refreshWorktrees },
  { id: "approvals", label: trNoop("Automatic approvals"), icon: "shield-check", component: ApprovalsSection },
  // SessionsWindow.cpp:2235, 2269, 2273 insert Teams, Model servers and Build
  // folders before Voice input, in that order.
  { id: "teams", label: "Teams", icon: "users", component: TeamsSection },
  // Task #397: project memory, its search and the model search notice.
  { id: "memory", label: trNoop("Memory"), icon: "hard-drive", component: MemorySection },
  // zerus-un7n: multi-person hosts, their shared history and provider support.
  { id: "sharedhosts", label: trNoop("Shared hosts"), icon: "users", component: SharedHostsSection },
  { id: "modelservers", label: trNoop("Model servers"), icon: "hard-drive", component: ModelServersSection },
  { id: "disk", label: trNoop("Build folders"), icon: "trash-2", component: DiskSection },
  { id: "voice", label: trNoop("Voice input"), icon: "mic", component: VoiceSection, refresh: refreshVoice },
  { id: "keyboard", label: "Keyboard", icon: "keyboard", component: KeyboardSection },
  { id: "analytics", label: trNoop("Private analytics"), icon: "gauge", component: AnalyticsSection },
  { id: "extensions", label: trNoop("Extensions"), icon: "plug", component: ExtensionsSection },
  { id: "about", label: "About", icon: "info", component: AboutSection, refresh: refreshAbout },
];

/** The sections the client has so far, in Qt's order. */
export const available = (): Section[] => SECTIONS.filter((s) => s.component);

const params = new URLSearchParams(typeof location === "undefined" ? "" : location.search);
const initial = available().find((s) => s.id === params.get("section"))?.id ?? "appearance";
export const [section, setSection] = createSignal<SectionId>(initial);

/** Opens Settings at `id` (Qt's openUpdates, openKeyboard, openRecovery, openAbout). */
export function openSettings(id: SectionId = section()) {
  const target = available().find((s) => s.id === id) ?? available()[0];
  setSection(target.id);
  target.refresh?.();
  setPage("settings");
}
