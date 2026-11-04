import { createRoot } from 'solid-js';
import { describe, expect, it } from 'vitest';
import { createImageHandoff, fittedImage, imageSize } from './attachment-image';

function delayed() { let resolve!: () => void, reject!: () => void; const promise=new Promise<void>((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject}; }
describe('stable sent-image presentation',()=>{
 it('reserves wide, tall, small, large PNG and oriented JPEG rectangles before decode',()=>{
  for (const [width,height,w,h] of [[2560,720,96,27],[720,2560,18,64],[80,50,80,50],[2400,1200,96,48],[800,1200,128/3,64]]) {
   const size=imageSize({width,height});const fitted=fittedImage(size);
   expect(parseFloat(fitted.width)).toBeCloseTo(w);expect(parseFloat(fitted.height)).toBeCloseTo(h);
  }
  expect(imageSize({width:0,height:10})).toBeUndefined();expect(imageSize({width:1e9,height:1e9})).toBeUndefined();
 });
 it('keeps the local source through decoding and a failed recorded source',async()=>{
  const decoded=delayed();let dispose!:()=>void;
  const handoff=createRoot(stop=>{dispose=stop;return createImageHandoff(()=>decoded.promise);});
  handoff.local('local');const pending=handoff.remote('recorded');expect(handoff.url()).toBe('local');
  decoded.resolve();await pending;expect(handoff.url()).toBe('recorded');dispose();
  const failure=delayed();const second=createRoot(stop=>{dispose=stop;return createImageHandoff(()=>failure.promise);});
  second.local('local');const broken=second.remote('broken');failure.reject();await broken;expect(second.url()).toBe('local');dispose();
 });
 it('ignores stale decodes and completion after the card unmounts',async()=>{
  const one=delayed(),two=delayed();let dispose!:()=>void;
  const handoff=createRoot(stop=>{dispose=stop;return createImageHandoff(url=>url==='one'?one.promise:two.promise);});
  handoff.local('local');const first=handoff.remote('one'),second=handoff.remote('two');one.resolve();await first;expect(handoff.url()).toBe('local');
  dispose();two.resolve();await second;expect(handoff.url()).toBe('local');
 });
});
