import { flag, num, setSetting } from "../../../store/settings";
import { Group, NumberField, Row, SectionHead, ToggleRow } from "./ui";
import { tr, trNoop } from "../../../i18n";

// Settings > Processes (tray/src/SettingsPage.h:109-120, ProcessSettings.h):
// the tab's switch and its four timings in seconds, with Qt's ranges, step
// and one decimal. The Rust hub reads the five keys from the settings store
// at start and after every change of one (src-tauri processes.rs).

export const TIMINGS = [
  { key: "processes/activeSeconds", label: trNoop("Open Processes tab"), min: 1, max: 60 },
  { key: "processes/backgroundSeconds", label: trNoop("Background process count"), min: 5, max: 600 },
  { key: "processes/unknownSeconds", label: trNoop("Unconfirmed process output"), min: 1, max: 600 },
  { key: "processes/loadingSeconds", label: trNoop("Show loading indicator after"), min: 0, max: 5 },
] as const;

export function ProcessesSection() {
  const enabled = () => flag("processes/enabled");
  return (
    <>
      <SectionHead title={trNoop("Processes")} />
      <Group>
        <ToggleRow label={trNoop("Enable Processes tab")} checked={enabled()} onChange={(on) => void setSetting("processes/enabled", on)} />
        {TIMINGS.map((t) => {
          const id = `processes-${t.key.slice("processes/".length)}`;
          return (
            <Row label={t.label} for={id} disabled={!enabled()}>
              <NumberField id={id} label={t.label} value={num(t.key)} min={t.min} max={t.max} step={0.5} decimals={1} suffix="s"
                disabled={!enabled()} onChange={(seconds) => void setSetting(t.key, seconds)} />
            </Row>
          );
        })}
      </Group>
      <p class="set-foot">{tr("Off by default. When disabled, the tab is hidden and processes are not polled. Enable it to inspect running shells and background commands, read recorded output and stop selected processes. Only the selected session is inspected; opening Processes refreshes immediately. Output is read only while the tab is visible. Timings apply to this Zerus. Background refresh also controls session inspection while Terminal or Native UI is open; session overview and account limits refresh separately.")}</p>
    </>
  );
}
