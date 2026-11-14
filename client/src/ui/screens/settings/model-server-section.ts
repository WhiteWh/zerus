import { createSignal } from "solid-js";

// which server Settings > Model servers opens at (Qt's showServer):
// a name, "" for the first (or a new one when there is none), or "+new".
export const NEW_SERVER = "+new";
const [requested, setRequested] = createSignal<{ name: string; at: number }>({ name: "", at: 0 });
export { requested as requestedServer };
let count = 0;
export function openModelServer(name: string) { setRequested({ name, at: ++count }); }
