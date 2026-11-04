import {createSignal} from "solid-js";
import {invoke} from "@tauri-apps/api/core";
import {inTauri} from "../bridge";
import {notify} from "../store/actions";
import {replaceUserSkins} from "./runtime";
import {parseSkin,BUILTIN,validateSkin} from "./import";
import {resolveSkins,type SkinFile} from "./skin";
import { tr } from "../i18n";
interface Loaded{files:unknown[];errors:string[]}
const [files,setFiles]=createSignal<SkinFile[]>([]);export const importedFiles=files;
function install(loaded:Loaded){const valid:SkinFile[]=[];const errors=[...loaded.errors];for(const raw of loaded.files.slice(0,32)){try{valid.push(parseSkin(JSON.stringify(raw)));}catch(error){errors.push(String(error));}}
 const skins=valid.flatMap(file=>{try{return resolveSkins([...BUILTIN,...valid],[file.id]);}catch(error){errors.push(String(error));return [];}});const ids=new Set(skins.map(s=>s.id));setFiles(valid.filter(f=>ids.has(f.id)));replaceUserSkins(skins);if(errors.length)notify("error",errors.slice(0,3).join("\n"),true);
}
let pending:Promise<void>|undefined;
export function loadUserSkins(){return pending??=inTauri()?invoke<Loaded>("user_skins_load").then(install).catch(error=>{notify("error",tr("Could not load user skins: %1", String(error)),true);}):Promise.resolve();}
export async function importSkin(text:string){const result=validateSkin(text,files());if(!result.ok)throw new Error(result.error);if(!inTauri())throw new Error("Saving a skin needs the desktop app.");const loaded=await invoke<Loaded>("user_skin_import",{text});install(loaded);return result;}
export const openSkinsFolder=()=>invoke<void>("user_skins_open");
