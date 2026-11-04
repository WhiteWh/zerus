import {resolveSkins,compileSkin,type SkinFile,type ResolvedSkin} from "./skin";
import dark from "./skins/zerus-dark.json";
import light from "./skins/zerus-light.json";
import fjord from "./skins/fjord.json";
import paper from "./skins/paper.json";
export const BUILTIN=[dark,light,fjord,paper] as SkinFile[];
export const MAX_SKIN_BYTES=256*1024;
const ALLOWED=new Set(["schema","id","name","appearance","extends","tokens","components","layout"]);
const record=(v:unknown):v is Record<string,unknown>=>!!v&&typeof v==="object"&&!Array.isArray(v);
export function parseSkin(text:string):SkinFile {
 if(new TextEncoder().encode(text).length>MAX_SKIN_BYTES)throw new Error("A skin file must be at most 256 KiB.");
 let value:unknown;try{value=JSON.parse(text);}catch(error){throw new Error(`Not valid JSON: ${(error as Error).message}`);}
 if(!record(value))throw new Error("A skin file is one JSON object.");
 for(const key of Object.keys(value))if(!ALLOWED.has(key))throw new Error(`Unknown field “${key}”. A skin carries tokens, component variants and layout variants only, never CSS or code.`);
 if(value.schema!==1)throw new Error(`Unsupported schema ${String(value.schema)}. This Zerus reads skin schema 1.`);
 if(typeof value.id!=="string"||!/^[a-z0-9-]{1,40}$/.test(value.id))throw new Error("id: lowercase letters, digits and dashes");
 if(typeof value.name!=="string"||!value.name.trim()||value.name.length>40||/[\x00-\x1f\x7f]/.test(value.name))throw new Error("name: 1 to 40 characters.");
 if(BUILTIN.some(b=>b.id===value.id))throw new Error(`A built-in skin already uses the id “${value.id}”. Choose another id.`);
 if(value.extends!==undefined&&(typeof value.extends!=="string"||!/^[a-z0-9-]{1,40}$/.test(value.extends)))throw new Error("Invalid parent skin id.");
 for(const field of ["tokens","components","layout"] as const)if(value[field]!==undefined&&!record(value[field]))throw new Error(`${field}: expected an object.`);
 for(const group of Object.values(value.tokens??{}))if(!record(group))throw new Error("tokens: expected token groups to be objects.");
 return value as unknown as SkinFile;
}
export type Validation={ok:true;skin:ResolvedSkin;css:string;file:SkinFile;tokens:number}|{ok:false;error:string};
export function validateSkin(text:string,others:SkinFile[]=[]):Validation {
 try{const file=parseSkin(text);const library=[...others.filter(f=>f.id!==file.id),file];if(library.length>32)throw new Error("At most 32 user skins are supported.");const skin=resolveSkins([...BUILTIN,...library],[file.id])[0];return {ok:true,skin,css:compileSkin(skin),file,tokens:Object.values(file.tokens??{}).reduce((n,g)=>n+Object.keys(g).length,0)};}catch(error){return {ok:false,error:String((error as Error).message??error)};}
}
export const SAMPLE=JSON.stringify({schema:1,id:"ember",name:"Ember",appearance:"dark",extends:"zerus-dark",tokens:{color:{accent:"#f0a35e","accent-hover":"#f6bf8a","accent-muted":"#a8703f","accent-bg":"rgba(240,163,94,.12)","segment-on":"#3a2b1e","card-selected":"#2a2119","card-selected-border":"#6b4a2c",bubble:"#33261b","bubble-border":"#6b4a2c"},radius:{card:10,dialog:14}},components:{"status-badge":"dot"},layout:{rail:"labeled"}},null,2);
