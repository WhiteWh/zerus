// Deterministic, keyless model running inside the official release for adapter
// integration tests. This fixture is never loaded by the installed HGS host.
import { existsSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
const require = createRequire(process.env.HGS_DSH_TEST_PACKAGE + '/package.json');
const { LlmAdapter } = await import(pathToFileURL(require.resolve('@deepseek-ai/dsh-llm')));
const { defineTool } = await import(pathToFileURL(require.resolve('@deepseek-ai/dsh-tools')));
class Adapter extends LlmAdapter {
  failedRecovery = new Set();
  providerInfo(provider) { return { id: provider, name: 'HGS test model' }; }
  async listModels(provider) { return [{ provider, id: 'test-a', name: 'Test A' }, { provider, id: 'test-b', name: 'Test B' }]; }
  async resolveModel(provider,model) {
    const candidate = model === 'test-b' && this.holds && join(this.holds, 'MODEL_ADMISSION_HOLD');
    const gate = candidate && existsSync(candidate + '.entered') && candidate;
    if (gate && existsSync(gate)) writeFileSync(gate + '.resolving', '');
    for (let i = 0; gate && existsSync(gate) && i < 300; i++) await new Promise(resolve => setTimeout(resolve, 50));
    return { provider,id:model,name:model,inputModalities: model === 'test-b' ? ['text'] : ['text','image'],
    reasoning:{efforts:[{id:'off',name:'Off'},{id:'high',name:'High'}],defaultEffort:'high'} }; }
  async *stream(options) {
    if (this.holds) writeFileSync(join(this.holds, 'SETTINGS_MODEL_STREAMS'), '1\n', {flag:'a'});
    await new Promise(resolve => setTimeout(resolve, 120));
    const prompt = options.messages.filter(m => m.role === 'user' && m.source?.kind === 'user').at(-1);
    const text = prompt?.content?.filter(b => b.type === 'text').map(b => b.text).join('') ?? '';
    if (text.includes('INTERRUPT_WAIT_FIXTURE')) {
      await new Promise((resolve,reject) => {
        const timer=setTimeout(resolve,10000);
        const abort=()=>{clearTimeout(timer);reject(new DOMException('Interrupted','AbortError'));};
        if(options.signal?.aborted)abort();else options.signal?.addEventListener('abort',abort,{once:true});
      });
    }
    if (text.includes('PROVIDER_RECOVER_ONCE_FIXTURE') && !this.failedRecovery.has(text)) {
      this.failedRecovery.add(text);
      throw Object.assign(new Error('503 Service unavailable'), {code:'service_unavailable'});
    }
    if (text.includes('PROVIDER_CAPACITY_FIXTURE')) throw Object.assign(new Error('Selected model is at capacity. Please try a different model.'), {code:'model_capacity'});
    const last = options.messages.at(-1);
    if (text.startsWith('HGS_SELF_NATIVE_FIXTURE ') && last?.role !== 'tool') {
      const args=JSON.stringify({command:text.slice('HGS_SELF_NATIVE_FIXTURE '.length),description:'Read fixture native identity and launch a child'});
      yield {type:'block-start',index:0,blockType:'tool-call'};
      yield {type:'tool-call-delta',index:0,id:'self-call',name:'bash',argumentsDelta:args};
      yield {type:'block-end',index:0,block:{type:'tool-call',id:'self-call',name:'bash',arguments:args}};
      yield {type:'finish',reason:{kind:'tool-calls'}};return;
    }
    // zerus-ppd26: the same identity command as a background job, promoted
    // after a short foreground timeout, or run by a child agent. Once per
    // prompt: a later job or child notice must not start it again.
    const shellPath = [['HGS_IDENTITY_BACKGROUND_FIXTURE ', {run_in_background:true}],
      ['HGS_IDENTITY_PROMOTED_FIXTURE ', {timeoutMs:1000}], ['HGS_IDENTITY_CHILD_FIXTURE ', null]]
      .find(([mark]) => text.startsWith(mark));
    if (shellPath && !options.messages.slice(options.messages.indexOf(prompt) + 1).some(m => m.role === 'tool')) {
      const [mark, extra] = shellPath, command = text.slice(mark.length);
      const [name, args] = extra
        ? ['bash', JSON.stringify({command, description:'Shell identity path fixture', ...extra})]
        : ['subagent', JSON.stringify({description:'Shell identity child fixture', prompt:'HGS_IDENTITY_CHILD_CALL '+command})];
      yield {type:'block-start',index:0,blockType:'tool-call'};
      yield {type:'tool-call-delta',index:0,id:'identity-path-call',name,argumentsDelta:args};
      yield {type:'block-end',index:0,block:{type:'tool-call',id:'identity-path-call',name,arguments:args}};
      yield {type:'finish',reason:{kind:'tool-calls'}};return;
    }
    // The child's own call: the native harness appends its parent instructions
    // as a later text block, so only the first block is the command.
    const childCall = prompt?.content?.find(block => block.type === 'text')?.text ?? '';
    if (childCall.startsWith('HGS_IDENTITY_CHILD_CALL ') && last?.role !== 'tool') {
      const args=JSON.stringify({command:childCall.slice('HGS_IDENTITY_CHILD_CALL '.length),description:'Shell identity child call'});
      yield {type:'block-start',index:0,blockType:'tool-call'};
      yield {type:'tool-call-delta',index:0,id:'identity-child-call',name:'bash',argumentsDelta:args};
      yield {type:'block-end',index:0,block:{type:'tool-call',id:'identity-child-call',name:'bash',arguments:args}};
      yield {type:'finish',reason:{kind:'tool-calls'}};return;
    }
    if ((text.includes('SHELL_BACKGROUND_FIXTURE') || text.includes('SHELL_FOREGROUND_FIXTURE')) && last?.role !== 'tool') {
      const background=text.includes('SHELL_BACKGROUND_FIXTURE');
      const args=JSON.stringify({command:background?"printf 'independent-output\\n'; sleep 30":"printf 'foreground-output\\n'; exit 7",description:'Shell process test',run_in_background:background});
      yield {type:'block-start',index:0,blockType:'tool-call'};
      yield {type:'tool-call-delta',index:0,id:'shell-call',name:'bash',argumentsDelta:args};
      yield {type:'block-end',index:0,block:{type:'tool-call',id:'shell-call',name:'bash',arguments:args}};
      yield {type:'finish',reason:{kind:'tool-calls'}};return;
    }
    if (text.includes('SHELL_READ_FIXTURE') && last?.role !== 'tool') {
      yield {type:'block-start',index:0,blockType:'tool-call'};
      yield {type:'tool-call-delta',index:0,id:'read-call',name:'hgs_test_job_read',argumentsDelta:'{}'};
      yield {type:'block-end',index:0,block:{type:'tool-call',id:'read-call',name:'hgs_test_job_read',arguments:'{}'}};
      yield {type:'finish',reason:{kind:'tool-calls'}};return;
    }
    if (text.includes('APPROVAL') && last?.role !== 'tool') {
      yield {type:'block-start',index:0,blockType:'tool-call'};
      yield {type:'tool-call-delta',index:0,id:'approval-call',name:'hgs_test_approval',argumentsDelta:'{}'};
      yield {type:'block-end',index:0,block:{type:'tool-call',id:'approval-call',name:'hgs_test_approval',arguments:'{}'}};
      yield {type:'finish',reason:{kind:'tool-calls'}};return;
    }
    // One child per prompt: a continuable child's completion reaches the
    // parent as a later message of its own, after the call's tool result.
    if (text.includes('SUBAGENT') && !options.messages.slice(options.messages.indexOf(prompt) + 1).some(m => m.role === 'tool')) {
      const args=JSON.stringify({description:'Native child fixture',prompt:'Child result fixture'+(text.match(/ (?:HOLD|DISPOSING|CORRUPT|UNRECORDED)_CHILD_\w+/)?.[0]??'')});
      yield {type:'block-start',index:0,blockType:'tool-call'};
      yield {type:'tool-call-delta',index:0,id:'child-call',name:'subagent',argumentsDelta:args};
      yield {type:'block-end',index:0,block:{type:'tool-call',id:'child-call',name:'subagent',arguments:args}};
      yield {type:'finish',reason:{kind:'tool-calls'}};return;
    }
    if (text.includes('UNCOMPOSED_ROOT_FIXTURE') && last?.role !== 'tool') {
      const args=JSON.stringify({sessionId:text.match(/webhook-[0-9a-f-]{36}/)?.[0]??''});
      yield {type:'block-start',index:0,blockType:'tool-call'};
      yield {type:'tool-call-delta',index:0,id:'root-call',name:'hgs_test_root',argumentsDelta:args};
      yield {type:'block-end',index:0,block:{type:'tool-call',id:'root-call',name:'hgs_test_root',arguments:args}};
      yield {type:'finish',reason:{kind:'tool-calls'}};return;
    }
    // The official Schedule tool, when its optional bundle is loaded.
    if (text.includes('SCHEDULE_FIXTURE') && last?.role !== 'tool') {
      const modern = typeof this.schedule?.()?.edit === 'function';
      const start = new Date(Date.now() + 3600000).toISOString().replace(/[-:]/g, '').replace(/\.\d{3}Z$/, 'Z');
      const args=JSON.stringify({prompt:'Scheduled fixture check',title:'Fixture check',...(modern
        ? { rrule:`DTSTART:${start}\nRRULE:FREQ=HOURLY`, target:'this_session' } : {every_seconds:3600})});
      yield {type:'block-start',index:0,blockType:'tool-call'};
      yield {type:'tool-call-delta',index:0,id:'schedule-call',name:'schedule_create',argumentsDelta:args};
      yield {type:'block-end',index:0,block:{type:'tool-call',id:'schedule-call',name:'schedule_create',arguments:args}};
      yield {type:'finish',reason:{kind:'tool-calls'}};return;
    }
    if (text.includes('QUESTION') && last?.role !== 'tool') {
      const args=JSON.stringify({ questions:[{id:'choice',question:'Which option?',options:[{label:'First'},{label:'Second'}]}] });
      yield {type:'block-start',index:0,blockType:'tool-call'};
      yield {type:'tool-call-delta',index:0,id:'question-call',name:'ask_user_question',argumentsDelta:args};
      yield {type:'block-end',index:0,block:{type:'tool-call',id:'question-call',name:'ask_user_question',arguments:args}};
      yield {type:'finish',reason:{kind:'tool-calls'}};return;
    }
    const reply=text.includes('ATTACHMENT_ORDER_FIXTURE')
      ? JSON.stringify(prompt.content.map(block => block.type === 'text' ? block.text : block.type))
      : 'Native reply: '+text;
    yield {type:'block-start',index:0,blockType:'reasoning'};
    yield {type:'reasoning-delta',index:0,text:'PRIVATE_TEST_REASONING'};
    yield {type:'block-end',index:0,block:{type:'reasoning',text:'PRIVATE_TEST_REASONING'}};
    yield {type:'block-start',index:1,blockType:'text'};
    yield {type:'text-delta',index:1,text:reply};
    yield {type:'block-end',index:1,block:{type:'text',text:reply}};
    yield {type:'usage',usage:{inputTokens:4,outputTokens:5}};
    yield {type:'finish',reason:{kind:'stop'}};
  }
}
export const inject=['llm','goals','tools','approval','jobs','agents'];
export function apply(ctx,config) {
  const adapter = new Adapter();
  adapter.holds = config?.holds;
  adapter.schedule = () => ctx.get('schedule');
  ctx.llm.registerAdapter(['hgs-test'],adapter);
  // Instrument the SAME upstream serializer, without supplying a second queue.
  // The second entry proves an image prompt reached admission while settings
  // was blocked in native model resolution inside the first queue entry.
  ctx.inject(['sessionController'], scope => {
    const control = scope.sessionController.agents, serialize = control.serializeImageAdmission;
    let entered = 0;
    control.serializeImageAdmission = function(agent, operation) {
      const gate = config?.holds && join(config.holds, 'MODEL_ADMISSION_HOLD');
      if (gate && existsSync(gate) && ++entered === 2) writeFileSync(gate + '.queued', '');
      if (!gate || !existsSync(gate)) entered = 0;
      return serialize.call(this, agent, () => {
        if (gate && existsSync(gate)) writeFileSync(gate + '.entered', '');
        return operation();
      });
    };
    return () => { control.serializeImageAdmission = serialize; };
  });
  // Hold a marked child's first step, before it records its subagent
  // descriptor, until the test creates the release file (as a slow checkpoint).
  // An ARMED hold also arms the child's goal while it waits, so a test can see
  // a disarm, and pauses it again on release so no goal round follows.
  ctx.on('agent/pre-step',async(request,next)=>{
    const {agent}=request;
    const hold=agent?.session.header.origin==='subagent'&&JSON.stringify(request.messages??[]).match(/HOLD_CHILD_\w+/)?.[0];
    const armed=hold&&hold.startsWith('HOLD_CHILD_ARMED_')&&config?.holds&&!existsSync(join(config.holds,hold))&&ctx.goals.get(agent);
    if(armed)ctx.goals.resume(agent,{id:armed.id,revision:armed.revision});
    for(let i=0;hold&&config?.holds&&i<300&&!existsSync(join(config.holds,hold));i++){
      if(i===0)writeFileSync(join(config.holds,hold+'.waiting'),'');
      await new Promise(resolve=>setTimeout(resolve,50));
    }
    const goal=armed&&ctx.goals.get(agent);
    if(goal?.phase==='active')ctx.goals.pause(agent,{id:goal.id,revision:goal.revision});
    return next();
  });
  // Damage (CORRUPT) or drop (UNRECORDED) the subagent descriptor of the child
  // a marked parent turn creates, as damaged storage would; the child's steps
  // still run. The engine records a descriptor when it creates the child, so
  // the parent's step arms the session class's append for that one record.
  const damages=new Map();
  ctx.on('agent/pre-step',(request,next)=>{
    const session=request.agent?.session;
    const damage=session&&session.header.origin!=='subagent'&&JSON.stringify(request.messages??[]).match(/(CORRUPT|UNRECORDED)_CHILD_\w+/)?.[1];
    if(damage){
      damages.set(session.id,damage);
      const prototype=Object.getPrototypeOf(session);
      if(!Object.hasOwn(prototype,'hgsTestAppend')){
        const append=prototype.append;
        Object.defineProperty(prototype,'hgsTestAppend',{value:append});
        prototype.append=function(type,data,...opts){
          const parent=type==='subagent/descriptor'&&this.header?.origin==='subagent'?this.header.parentSession:undefined;
          const armed=parent&&damages.get(parent);
          if(!armed)return append.call(this,type,data,...opts);
          damages.delete(parent);
          if(armed==='CORRUPT')return append.call(this,type,{...data,provider:null},...opts);
        };
      }
    }
    return next();
  });
  // Native disposal unloads an agent's Cordis scope before the agent leaves
  // the live registry. Keep a marked child in that window, as a slow scope
  // teardown would, and report it with a marker file until the test releases it.
  // Another marker reports when the child has left the registry.
  const disposing=new WeakMap();
  ctx.on('agent/pre-step',(request,next)=>{
    const agent=request.agent;
    const hold=agent?.session.header.origin==='subagent'&&JSON.stringify(request.messages??[]).match(/DISPOSING_CHILD_\w+/)?.[0];
    if(hold&&config?.holds&&!disposing.has(agent)){
      disposing.set(agent,hold);
      agent.ctx.effect(()=>async()=>{
        writeFileSync(join(config.holds,hold+'.disposing'),'');
        for(let i=0;i<300&&!existsSync(join(config.holds,hold));i++)await new Promise(resolve=>setTimeout(resolve,50));
      },'hgs-test.disposal-hold');
    }
    return next();
  });
  ctx.on('agent/disposed',({agent})=>{
    const hold=disposing.get(agent);
    if(hold)writeFileSync(join(config.holds,hold+'.disposed'),'');
  });
  // While ACCOUNT_HOLD exists, account reads, which bridge reads await, report
  // ACCOUNT_HOLD.waiting and wait for its removal, so agents can change meanwhile.
  ctx.inject(['deepseekAccount'],scope=>{
    const account=scope.deepseekAccount,getState=account.getState;
    account.getState=async function(...args){
      const gate=config?.holds&&join(config.holds,'ACCOUNT_HOLD');
      if(gate&&existsSync(gate))writeFileSync(gate+'.waiting','');
      for(let i=0;gate&&i<300&&existsSync(gate);i++)await new Promise(resolve=>setTimeout(resolve,50));
      return getState.apply(this,args);
    };
    return()=>{account.getState=getState;};
  });
  ctx.tools.register(defineTool({name:'hgs_test_job_read',description:'Verify the model output cursor',parameters:{},
    output:{schema:{type:'string'},render:(_args,value)=>[{type:'text',text:value}]},
    async execute(_args,exec){const owner=exec.agent.session.id;const job=ctx.jobs.list(owner).find(j=>j.owner===owner&&j.kind==='bash');return ctx.jobs.read(job.id,owner).chunks.map(c=>c.text).join('');}
  }));
  // A top-level agent on its own route, as a webhook creates one: no parent,
  // and no model selection, which upstream installs only while composing one.
  ctx.tools.register(defineTool({name:'hgs_test_root',description:'Create an uncomposed top-level agent',
    parameters:{sessionId:{type:'string',required:true}},
    output:{schema:{type:'string'},render:(_args,value)=>[{type:'text',text:value}]},
    async execute(args,exec){
      await ctx.agents.create({sessionId:args.sessionId,agentOptions:{provider:'hgs-test',model:'test-b'},meta:{cwd:exec.agent.session.header.cwd}});
      return args.sessionId;
    }
  }));
  ctx.tools.register(defineTool({name:'hgs_test_approval',description:'Keyless approval fixture',parameters:{},
    output:{schema:{type:'string'},render:(_args,value)=>[{type:'text',text:value}]},
    async execute(_args,exec){return ctx.approval.request({agent:exec.agent,toolName:'hgs_test_approval',callId:exec.callId,reason:'Test an explicit native decision',signal:exec.signal});}
  }));
  ctx.on('agent/created',({agent,source})=>{
    // A native fork starts from its source's seeded history, goal included.
    if (source !== 'startup' || ctx.goals.get(agent)) return;
    const created=ctx.goals.create(agent,{objective:'Test the native adapter',maxGoalRounds:8});
    ctx.goals.pause(agent,{id:created.id,revision:created.revision});
  });
}
