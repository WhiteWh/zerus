// The native local API renews its own OAuth grant. No session is created or resumed.
import {spawn} from 'node:child_process';
import net from 'node:net';
const output = {identity:{},windows:[],source:'Kimi Code',status:'unavailable'};
const listener=net.createServer();
await new Promise((resolve,reject)=>{listener.once('error',reject);listener.listen(0,'127.0.0.1',resolve);});
const port=listener.address().port;await new Promise(resolve=>listener.close(resolve));
// A server that already runs keeps its own address and ignores --port; use the one it reports.
let base=`http://127.0.0.1:${port}`;
const child=spawn('kimi',['web','--host','127.0.0.1','--port',String(port),'--no-open'],{stdio:['ignore','pipe','pipe']});
try {
  const token=await new Promise((resolve,reject)=>{
    let buffer='';const timer=setTimeout(()=>reject(new Error('startup')),5000);
    const read=chunk=>{
      buffer+=chunk.toString().replace(/\x1b\[[0-9;]*m/g,'');
      const local=buffer.match(/(http:\/\/127\.0\.0\.1:\d+)\/#token=([A-Za-z0-9_-]+)/);
      if(local)base=local[1];
      const match=local?[local[0],local[2]]:buffer.match(/#token=([A-Za-z0-9_-]+)/)??buffer.match(/Token:\s+([A-Za-z0-9_-]{16,})/);
      if(match){clearTimeout(timer);resolve(match[1]);}
      if(buffer.length>65536){clearTimeout(timer);reject(new Error('output'));}
    };
    child.stdout.on('data',read);child.stderr.on('data',read);child.once('error',()=>{clearTimeout(timer);reject(new Error('launch'));});
    child.once('close',()=>{clearTimeout(timer);reject(new Error('exit'));});
  });
  const get=async path=>{
    const r=await fetch(`${base}/api/v1/oauth/${path}`,{headers:{Authorization:`Bearer ${token}`},redirect:'error',signal:AbortSignal.timeout(9000)});
    if(!r.ok)throw new Error('request');return (await r.json()).data;
  };
  // One failed read must not hide the other: a plan without quota still has a verified sign-in.
  const read=async path=>{try{return await get(path);}catch{return null;}};
  // Sequence these so the first native call finishes token renewal before the second.
  const usage=await read('usage');const profile=await read('userinfo');
  const clean=v=>typeof v==='string'?v.replace(/[\x00-\x1f\x7f]/g,'').slice(0,256):'';
  if(profile?.kind==='ok'){
    const p=profile.userInfo??{};
    output.identity={name:clean(p.nickname),email:clean(p.email),plan:clean(p.userLevelName),auth_method:'Kimi Code'};
    // A stable account field verifies this token; the quota answer only decides the quota status.
    if([p.email,p.userId,p.globalId].some(v=>typeof v==='string'&&v.length>0))output.auth_status='signed_in';
  }
  if(usage?.kind==='ok') {
    const usages=usage.quota?.usages??{};
    for(const [key,label,minutes] of [['limit5h','',300],['limit7d','',10080],['monthTotal','Monthly',null],['monthCode','Monthly code',null]]) {
      const w=usages[key];if(w && typeof w.usedRatio==='number' && Number.isFinite(w.usedRatio) && w.usedRatio>=0)
        output.windows.push({id:key,label,used_percent:w.usedRatio*100,window_minutes:minutes,resets_at:w.resetAt??null});
    }
    if(output.windows.length)output.status='ok';
  }
} catch {}
finally {child.kill('SIGTERM');setTimeout(()=>child.kill('SIGKILL'),500).unref();}
process.stdout.write(JSON.stringify(output));
