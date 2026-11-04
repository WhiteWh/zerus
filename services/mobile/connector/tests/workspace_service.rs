//! Opt-in Linux fixture: no system manager, relay, native model or owner files.
//! Build the dedicated example normally and pass its exact artifact path.
#![cfg(all(target_os = "linux", feature = "test-hooks"))]
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use zerus_mobile_connector::{
    private_state::Store,
    workspace_cli,
    workspace_service::{self as service, TestPaths},
};
const NODE: &str = "00000000-0000-4000-8000-000000000350";
const MANAGER: &str = r#"#!/usr/bin/python3
import json,os,subprocess,sys,time
from pathlib import Path
r=Path(__file__).parent;s=r/'manager.json';q=json.loads(s.read_text());a=sys.argv[1:]
with (r/'manager.calls').open('a') as f:f.write(json.dumps(a)+'\n')
assert a[0]=='--user'
def durable(path,value):
 fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_TRUNC|os.O_NOFOLLOW,0o600)
 with os.fdopen(fd,'w') as f:f.write(json.dumps(value));f.flush();os.fsync(f.fileno())
 d=os.open(r,os.O_RDONLY|os.O_DIRECTORY)
 try:os.fsync(d)
 finally:os.close(d)
if a[1]=='daemon-reload':
 p=r/'systemd/zerus-mobile-connector.service.d/70-zerus-workspace-connector.conf'
 q['loaded']=[l[len('ExecStart='):].split(' --config ')[0] for l in p.read_text().splitlines() if l.startswith('ExecStart=') and l!='ExecStart='][-1] if p.exists() else str(r/'old/zerus-mobile-connector')
 if q.get('reorder_requires'):q['properties']['Requires']=' '.join(reversed(q['properties']['Requires'].split()))
 s.write_text(json.dumps(q))
elif a[1]=='start':
 assert a[2]=='zerus-mobile-connector.service'
 intent=r/'manager.start-intent.json';durable(intent,{'pending':True})
 p=None;registered=False
 try:
  p=subprocess.Popen([q['loaded'],'--config',str(r/'connector.json')],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
  if q.get('fail_after_spawn'):raise RuntimeError('synthetic post-spawn custody fault')
  start=Path('/proc/'+str(p.pid)+'/stat').read_text().rsplit(') ',1)[1].split()[19]
  q.setdefault('owned_starts',[]).append({'pid':p.pid,'start':start,'image':q['loaded']})
  q['pid']=p.pid;q['invocation']='synthetic-new';q['starts']=q.get('starts',0)+1
  durable(s,q);registered=True
  durable(intent,{'pending':False,'outcome':'registered','pid':p.pid,'start':start})
 except BaseException:
  if p is not None and not registered:
   p.terminate()
   try:p.wait(timeout=3)
   except subprocess.TimeoutExpired:p.kill();p.wait(timeout=3)
   durable(intent,{'pending':False,'outcome':'reaped-after-error','pid':p.pid})
  raise
elif a[1]=='show':
 if q.get('show_delay'):time.sleep(q['show_delay'])
 q['shows']=q.get('shows',0)+1;s.write_text(json.dumps(q))
 if q.get('refuse_on_show')==q['shows']:q.setdefault('properties',{})['ConsistsOf']='synthetic-native.scope';s.write_text(json.dumps(q))
 pid=q['pid'];alive=False
 try:alive=Path('/proc/'+str(pid)+'/stat').read_text().rsplit(') ',1)[1].split()[0]!='Z'
 except FileNotFoundError:pass
 cg=r/'cgroup';cg.mkdir(exist_ok=True)
 group=q.get('group','/fixture/zerus-mobile-connector.service')
 (cg/'proc-cgroup').write_text('0::'+group+'\n')
 directory=cg/'fixture/zerus-mobile-connector.service';directory.mkdir(parents=True,exist_ok=True)
 (directory/'cgroup.procs').write_text(str(pid)+'\n'+('999999\n' if q.get('extra_process') else ''))
 if q.get('extra_scope'):(directory/'native.scope').mkdir(exist_ok=True)
 command=q.get('override') or q['loaded'];argv=command+' --config '+str(r/'connector.json')
 drop=r/'systemd/zerus-mobile-connector.service.d/70-zerus-workspace-connector.conf'
 values={'ControlGroup':group,'LoadState':'loaded','ActiveState':'active' if alive else 'inactive','SubState':'running' if alive else 'dead','MainPID':str(pid) if alive else '0','InvocationID':q['invocation'] if alive else '', 'KillMode':'process','Restart':'on-failure','Type':'simple','RemainAfterExit':'no','ExitType':'main','FailureAction':'none','SuccessAction':'none','WatchdogUSec':'0','RuntimeMaxUSec':'infinity','Result':'success','ExecMainStatus':'0','StopWhenUnneeded':'no','MemoryMax':'268435456','MemoryHigh':'infinity','CPUQuotaPerSecUSec':'infinity','TasksMax':'64','FragmentPath':str(r/'base.service'),'DropInPaths':str(drop) if drop.exists() else '', 'ExecStart':'{ path='+command+' ; argv[]='+argv+' ; ignore_errors=no ; }'}
 values.update(q.get('properties',{}))
 for k in a[a.index('--property')+1].split(','):
  v=values.get(k,'')
  if v=='' and k in ['ExecStop','ExecStopPost','ExecStartPre','ExecStartPost','ExecCondition','EnvironmentFiles']:continue
  print(k+'='+v)
else:raise AssertionError('Fixture refuses every stop/restart/kill job')
"#;
const NATIVE: &str = r#"#!/usr/bin/python3
import json,os,sys,time
from pathlib import Path
r=Path(__file__).parent;a=sys.argv[1:]
assert a==['send','codex/example/service-fixture','--json'],a
p=json.load(sys.stdin)
assert p=={'request_id':'00000000-0000-4000-8000-000000000351','expected_run_id':'fixture-run','expected_conversation_id':'fixture-conversation','text':'fixture held message'}
(r/'native-ready').write_text(str(os.getpid()))
with (r/'native-calls').open('a') as f:f.write('send\n')
end=time.monotonic()+40
while not (r/'release').exists():
 if time.monotonic()>end:raise SystemExit(3)
 time.sleep(.02)
(r/'native-finished').write_text('finished')
print(json.dumps({'status':'submitted','request_id':p['request_id'],'name':a[1],'run_id':p['expected_run_id'],'conversation_id':p['expected_conversation_id']}))
"#;
fn write(path: &Path, bytes: &[u8], mode: u32) {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    f.set_permissions(std::fs::Permissions::from_mode(mode))
        .unwrap();
    f.write_all(bytes).unwrap();
    f.sync_all().unwrap();
}
fn value(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn save(path: &Path, v: &Value) {
    std::fs::write(path, serde_json::to_vec(v).unwrap()).unwrap()
}
async fn until(mut predicate: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(8);
    while !predicate() {
        assert!(
            Instant::now() < end,
            "Synthetic fixture did not reach its owned boundary"
        );
        tokio::time::sleep(Duration::from_millis(20)).await
    }
}
struct Fixture {
    root: tempfile::TempDir,
    child: Child,
    config: PathBuf,
    private: PathBuf,
    dropin: PathBuf,
    old: PathBuf,
    new: PathBuf,
    images: Vec<PathBuf>,
    managed: std::cell::RefCell<Vec<OwnedHandle>>,
    evidence: PathBuf,
}
struct OwnedHandle {
    pid: i32,
    start: String,
    fd: rustix::fd::OwnedFd,
}
impl Fixture {
    async fn new() -> Self {
        let evidence = PathBuf::from(
            std::env::var_os("ZERUS_WORKSPACE_SERVICE_EVIDENCE")
                .expect("Pass a private pre-created fixture custody directory"),
        );
        zerus_mobile_connector::fsguard::check_private_directory(&evidence).unwrap();
        let root = tempfile::Builder::new()
            .prefix("zerus-service-")
            .tempdir()
            .unwrap();
        let r = root.path();
        assert!(
            !r.starts_with("/tmp"),
            "Qualification must provide a private /w TMPDIR; setup refusal is not causal RED"
        );
        std::fs::create_dir(r.join("old")).unwrap();
        std::fs::create_dir(r.join("new")).unwrap();
        let source = PathBuf::from(
            std::env::var_os("ZERUS_WORKSPACE_SERVICE_FIXTURE")
                .expect("Pass the exact normal Cargo example artifact"),
        );
        let old = r.join("old/zerus-mobile-connector");
        let new = r.join("new/zerus-mobile-connector");
        std::fs::copy(&source, &old).unwrap();
        std::fs::copy(&source, &new).unwrap();
        write(&r.join("manager"), MANAGER.as_bytes(), 0o755);
        write(&r.join("native"), NATIVE.as_bytes(), 0o755);
        let config = r.join("connector.json");
        let private = r.join("workspace-private");
        let cfg = json!({"server_url":"https://relay.example.test","node_token":"synthetic-service-bearer","state_dir":r.join("old-receipts"),"hgs_path":r.join("native"),"fixture_target":r.join("created")});
        workspace_cli::replace_config(&config, "", &cfg).unwrap();
        write(&r.join("base.service"),format!("[Unit]\nDescription=Synthetic ordinary user unit\n[Service]\nExecStart={} --config {}\nKillMode=process\n",old.display(),config.display()).as_bytes(),0o644);
        let log = File::create(r.join("connector.log")).unwrap();
        let child = Command::new(&old)
            .args(["--config", config.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let dropin =
            r.join("systemd/zerus-mobile-connector.service.d/70-zerus-workspace-connector.conf");
        let fixture = Self {
            root,
            child,
            config,
            private,
            dropin,
            old,
            images: vec![new.clone()],
            managed: std::cell::RefCell::new(Vec::new()),
            evidence,
            new,
        };
        save(
            &fixture.path("manager.json"),
            &json!({"pid":fixture.child.id(),"loaded":fixture.old,"invocation":"synthetic-old","starts":0}),
        );
        until(|| fixture.path("native-ready").exists()).await;
        fixture
    }
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
    fn paths(&self) -> TestPaths {
        TestPaths {
            manager: self.path("manager"),
            binary: self.new.clone(),
            dropin: self.dropin.clone(),
            cgroup: self.path("cgroup"),
        }
    }
    async fn drain(&self) -> Option<Value> {
        service::test_scope(
            self.paths(),
            service::test_drain(&self.config, &self.private, NODE),
        )
        .await
        .unwrap()
    }
    fn signals(&self) -> usize {
        std::fs::read_to_string(self.path("connector.log"))
            .unwrap()
            .matches("Connector stopping after the request in progress")
            .count()
    }
    async fn finish(&mut self) {
        std::fs::write(self.path("release"), b"release").unwrap();
        until(|| self.child.try_wait().unwrap().is_some()).await;
        assert!(self.child.try_wait().unwrap().unwrap().success());
    }
    fn mutate(&self, f: impl FnOnce(&mut Value)) {
        let p = self.path("manager.json");
        let mut v = value(&p);
        f(&mut v);
        save(&p, &v)
    }
    async fn stop_new(&self) {
        let state = value(&self.path("manager.json"));
        let pid = state["pid"].as_u64().unwrap() as i32;
        if pid == self.child.id() as i32 {
            return;
        }
        let fd = rustix::process::pidfd_open(
            rustix::process::Pid::from_raw(pid).unwrap(),
            rustix::process::PidfdFlags::empty(),
        )
        .unwrap();
        let owner = state["owned_starts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["pid"].as_i64() == Some(pid as i64))
            .unwrap();
        assert_eq!(owned_start(pid).as_deref(), owner["start"].as_str());
        assert!(self.images.iter().any(|p| owner["image"] == json!(p)
            && std::fs::canonicalize(format!("/proc/{pid}/exe"))
                .ok()
                .as_ref()
                == Some(p)));
        rustix::process::pidfd_send_signal(&fd, rustix::process::Signal::TERM).unwrap();
        // Tokio 1.53.3's replacement requires unsafe code, forbidden by this crate.
        // This OwnedFd keeps its descriptor and file description open and stable
        // until AsyncFd drops. Only readable borrows it; it is never swapped,
        // closed, extracted or accessed via get_mut/into_inner.
        #[allow(deprecated)]
        let fd = tokio::io::unix::AsyncFd::new(fd).unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), fd.readable())
            .await
            .unwrap()
            .unwrap();
    }
    fn profile(&self, node: &str) -> zerus_mobile_connector::workspace::Profile {
        use zerus_mobile_crypto::{
            encode, Identity, MembershipAction, MembershipState, MembershipStatement, Role,
            SignedMembership,
        };
        let identity = Identity::generate(node.into(), Role::Computer).unwrap();
        let genesis = SignedMembership::sign(
            MembershipStatement {
                workspace_id: uuid::Uuid::new_v4().to_string(),
                revision: 0,
                previous_head: encode(&[0; 32]),
                issuer_device_id: node.into(),
                issuer_key_generation: 1,
                action: MembershipAction::Add,
                device: Some(identity.public().clone()),
                revoked_device_id: None,
            },
            &identity,
        )
        .unwrap();
        let membership = MembershipState::from_genesis(&genesis).unwrap();
        zerus_mobile_connector::workspace::Profile {
            server_url: "https://relay.example.test".into(),
            workspace_name: "Fixture".into(),
            computer_name: "Fixture".into(),
            node_token: encode(&[32; 32]),
            identity: identity.to_record(),
            historical_heads: BTreeMap::from([(0, membership.head.clone())]),
            membership,
            state_dir: self.private.join(format!("receipts-{node}")),
        }
    }
    fn replace_private(&self, name: &str, value: &Value) {
        let store = Store::lock(&self.private, false).unwrap();
        let expected = Store::revision(store.read(name).unwrap().as_deref());
        store
            .replace(name, &expected, &serde_json::to_vec(value).unwrap())
            .unwrap();
    }
    fn retire_profile(&self) {
        use zerus_mobile_crypto::{
            Identity, MembershipAction, MembershipStatement, SignedMembership,
        };
        let mut profile: zerus_mobile_connector::workspace::Profile =
            serde_json::from_value(value(&self.private.join("active.json"))).unwrap();
        let identity = Identity::from_record(&profile.identity).unwrap();
        let change = SignedMembership::sign(
            MembershipStatement {
                workspace_id: profile.membership.workspace_id.clone(),
                revision: profile.membership.revision + 1,
                previous_head: profile.membership.head.clone(),
                issuer_device_id: identity.public().device_id.clone(),
                issuer_key_generation: identity.public().key_generation,
                action: MembershipAction::Revoke,
                device: None,
                revoked_device_id: Some(identity.public().device_id.clone()),
            },
            &identity,
        )
        .unwrap();
        profile.membership.apply_next(&change).unwrap();
        profile
            .historical_heads
            .insert(profile.membership.revision, profile.membership.head.clone());
        self.replace_private("active.json", &serde_json::to_value(profile).unwrap());
        assert!(zerus_mobile_connector::workspace::is_verified_retired(&self.private).unwrap());
    }
    fn activate_as(&self, node: &str) -> String {
        self.activate_profile(self.profile(node))
    }
    fn activate_profile(&self, profile: zerus_mobile_connector::workspace::Profile) -> String {
        let node = profile.identity.device.device_id.clone();
        let store = Store::lock(&self.private, false).unwrap();
        let old = Store::revision(store.read("active.json").unwrap().as_deref());
        store
            .replace("active.json", &old, &serde_json::to_vec(&profile).unwrap())
            .unwrap();
        drop(store);
        let (mut cfg, old) = workspace_cli::read_config(&self.config).unwrap();
        cfg.as_object_mut().unwrap().remove("node_token");
        cfg["server_url"] = json!(profile.server_url);
        cfg["state_dir"] = json!(profile.state_dir);
        cfg["encrypted_workspace"] = json!({"version":1,"state_dir":self.private});
        let revision = workspace_cli::replace_config(&self.config, &old, &cfg).unwrap();
        service::test_activated(&self.private, &node, &revision).unwrap();
        revision
    }
    fn activate(&self) -> String {
        self.activate_as(NODE)
    }
    async fn start(&self, node: &str, revision: &str) -> Value {
        let status = service::test_scope(
            self.paths(),
            service::test_start(&self.config, &self.private, node, revision),
        )
        .await
        .unwrap()
        .unwrap();
        let state = value(&self.path("manager.json"));
        let pid = i32::try_from(state["pid"].as_u64().unwrap()).unwrap();
        let owner = state["owned_starts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["pid"].as_i64() == Some(pid as i64))
            .unwrap();
        let start = owner["start"].as_str().unwrap().to_owned();
        let fd = rustix::process::pidfd_open(
            rustix::process::Pid::from_raw(pid).unwrap(),
            rustix::process::PidfdFlags::empty(),
        )
        .unwrap();
        assert_eq!(owned_start(pid).as_deref(), Some(start.as_str()));
        assert!(self.images.iter().any(|p| owner["image"] == json!(p)
            && std::fs::canonicalize(format!("/proc/{pid}/exe"))
                .ok()
                .as_ref()
                == Some(p)));
        if !self
            .managed
            .borrow()
            .iter()
            .any(|p| p.pid == pid && p.start == start)
        {
            self.managed
                .borrow_mut()
                .push(OwnedHandle { pid, start, fd });
        }
        status
    }
    async fn cooperative_stop(&self, revision: &str, generation: &str) {
        let result = service::test_scope(
            self.paths(),
            zerus_mobile_connector::workspace::manage(
                "stop",
                &self.config,
                json!({"expected_revision":revision,"generation":generation}),
                false,
            ),
        )
        .await
        .unwrap();
        assert_eq!(result["state"], "stopped");
        until(|| {
            value(&self.path("manager.json"))["pid"]
                .as_u64()
                .is_some_and(|p| owned_start(p as i32).is_none())
        })
        .await;
    }
}
fn owned_start(pid: i32) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = text
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    if fields.first() == Some(&"Z") {
        return None;
    }
    Some(fields.get(19)?.to_string())
}
fn owned_exited(pid: i32, expected: &str) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        Ok(text) => text.rsplit_once(") ").is_some_and(|(_, rest)| {
            let fields = rest.split_whitespace().collect::<Vec<_>>();
            fields
                .get(19)
                .is_some_and(|start| *start != expected || fields.first() == Some(&"Z"))
        }),
    }
}
// Test-only cleanup targets positively created private fixture images. An
// assertion can unwind at any point after manager start; no broad process scan.
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::write(self.path("release"), b"release");
        let end = Instant::now() + Duration::from_secs(3);
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // Retained pidfds remain exact even if an ordinary installed update
        // unlinks the old image while that owned process is still running.
        for process in self.managed.borrow().iter() {
            if owned_start(process.pid).as_deref() != Some(process.start.as_str()) {
                continue;
            }
            let _ = rustix::process::pidfd_send_signal(&process.fd, rustix::process::Signal::TERM);
            let end = Instant::now() + Duration::from_secs(3);
            while owned_start(process.pid).as_deref() == Some(process.start.as_str())
                && Instant::now() < end
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            if owned_start(process.pid).as_deref() == Some(process.start.as_str()) {
                let _ =
                    rustix::process::pidfd_send_signal(&process.fd, rustix::process::Signal::KILL);
                let end = Instant::now() + Duration::from_secs(3);
                while owned_start(process.pid).as_deref() == Some(process.start.as_str())
                    && Instant::now() < end
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        let state = std::fs::read(self.path("manager.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        if let Some(starts) = state.as_ref().and_then(|q| q["owned_starts"].as_array()) {
            assert!(starts.len() <= 8, "Fixture ownership bound exceeded");
            for q in starts {
                let Some(pid) = q["pid"].as_u64().and_then(|p| i32::try_from(p).ok()) else {
                    continue;
                };
                let Some(start) = q["start"].as_str() else {
                    continue;
                };
                if owned_start(pid).as_deref() != Some(start) {
                    continue;
                }
                let fd = rustix::process::Pid::from_raw(pid).and_then(|p| {
                    rustix::process::pidfd_open(p, rustix::process::PidfdFlags::empty()).ok()
                });
                if owned_start(pid).as_deref() != Some(start)
                    || !self.images.iter().any(|p| {
                        q["image"] == json!(p)
                            && std::fs::canonicalize(format!("/proc/{pid}/exe"))
                                .ok()
                                .as_ref()
                                == Some(p)
                    })
                {
                    continue;
                }
                if let Some(fd) = fd {
                    let _ = rustix::process::pidfd_send_signal(&fd, rustix::process::Signal::TERM);
                    let end = Instant::now() + Duration::from_secs(3);
                    while owned_start(pid).as_deref() == Some(start) && Instant::now() < end {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    if owned_start(pid).as_deref() == Some(start) {
                        let _ =
                            rustix::process::pidfd_send_signal(&fd, rustix::process::Signal::KILL);
                        let end = Instant::now() + Duration::from_secs(3);
                        while owned_start(pid).as_deref() == Some(start) && Instant::now() < end {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }
                }
            }
        }
        let owned = state
            .as_ref()
            .and_then(|q| q["owned_starts"].as_array())
            .cloned()
            .unwrap_or_default();
        let mut unknown = Vec::new();
        for q in &owned {
            let exited = q["pid"]
                .as_i64()
                .zip(q["start"].as_str())
                .is_some_and(|(p, s)| owned_exited(p as i32, s));
            if !exited {
                unknown.push(q.clone());
            }
        }
        for process in self.managed.borrow().iter() {
            if !owned_exited(process.pid, &process.start) {
                unknown.push(
                    json!({"pid":process.pid,"start":process.start,"source":"retained-pidfd"}),
                );
            }
        }
        if state.is_none()
            && std::fs::read_to_string(self.path("manager.calls"))
                .is_ok_and(|calls| calls.contains("\"start\""))
        {
            unknown.push(json!({"start_state_unavailable":true}));
        }
        let intent_path = self.path("manager.start-intent.json");
        let start_intent = std::fs::read(&intent_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        if intent_path.exists() && start_intent.as_ref().is_none_or(|v| v["pending"] != false) {
            unknown.push(json!({"unresolved_start_intent":true}));
        }
        let old_exit = self.child.try_wait().ok().flatten();
        if old_exit.is_none() {
            unknown.push(json!({"old_pid": self.child.id()}));
        }
        let receipt = json!({"schema":1,"fixture":self.root.path(),"old_pid":self.child.id(),
            "old_exited":old_exit.is_some(),"managed":owned,
            "start_intent":start_intent,"retained_pidfds":self.managed.borrow().len(),"unknown":unknown});
        let path = self.evidence.join(format!(
            "{}.json",
            self.root.path().file_name().unwrap().to_string_lossy()
        ));
        let result = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .and_then(|mut file| {
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                file.write_all(&serde_json::to_vec(&receipt).unwrap())?;
                file.sync_all()
            });
        if result.is_err() || !receipt["unknown"].as_array().unwrap().is_empty() {
            eprintln!("WORKSPACE_SERVICE_FIXTURE_CUSTODY_FAILURE");
            if !std::thread::panicking() {
                panic!("Owned fixture cleanup or custody evidence failed");
            }
        }
    }
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn one_signal_drains_native_and_resumed_check_keeps_the_exact_outcome() {
    let mut f = Fixture::new().await;
    f.mutate(|v| {
        v["properties"] = json!({"Slice":"app.slice","Requires":"basic.target app.slice"});
        v["reorder_requires"] = json!(true)
    });
    let cfg = std::fs::read(&f.config).unwrap();
    assert_eq!(f.drain().await.unwrap()["phase"], "draining");
    assert_eq!(f.drain().await.unwrap()["phase"], "draining");
    assert_eq!(f.signals(), 1);
    assert!(!f.path("native-finished").exists());
    f.finish().await;
    assert!(f.drain().await.is_none());
    assert_eq!(f.signals(), 1);
    assert_eq!(std::fs::read(&f.config).unwrap(), cfg);
    let outcome = value(&f.path("fixture-outcome.json"));
    assert_eq!(
        outcome,
        json!({"state":"completed","error":null,"result":{"status":"submitted","request_id":"00000000-0000-4000-8000-000000000351","name":"codex/example/service-fixture","run_id":"fixture-run","conversation_id":"fixture-conversation"}})
    );
    assert_eq!(
        std::fs::read_to_string(f.path("native-calls")).unwrap(),
        "send\n"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn lost_stop_reply_is_observed_without_resending() {
    let mut f = Fixture::new().await;
    let _lost = f.drain().await;
    let record = value(&f.private.join("legacy-service-handover.json"));
    assert_eq!(record["signal_reserved"], true);
    assert_eq!(
        service::status(
            &f.private,
            &workspace_cli::read_config(&f.config).unwrap().1,
            NODE
        )
        .unwrap()
        .unwrap()["phase"],
        "draining"
    );
    f.finish().await;
    assert!(f.drain().await.is_none());
    assert_eq!(f.signals(), 1);
}
#[tokio::test(flavor = "current_thread")]
async fn substituted_main_process_and_declared_image_receive_no_signal() {
    let mut f = Fixture::new().await;
    let mut foreign = OwnedChild(Command::new("/usr/bin/sleep").arg("20").spawn().unwrap());
    f.mutate(|v| v["pid"] = json!(foreign.0.id()));
    assert_eq!(f.drain().await.unwrap()["phase"], "blocked");
    assert!(foreign.0.try_wait().unwrap().is_none());
    assert_eq!(f.signals(), 0);
    f.mutate(|v| {
        v["pid"] = json!(f.child.id());
        v["loaded"] = json!(f.new)
    });
    assert_eq!(f.drain().await.unwrap()["phase"], "blocked");
    assert_eq!(f.signals(), 0);
    foreign.0.kill().unwrap();
    foreign.0.wait().unwrap();
    f.finish().await;
}
#[tokio::test(flavor = "current_thread")]
async fn changed_invocation_and_config_never_authorize_another_signal() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.mutate(|v| v["invocation"] = json!("foreign-invocation"));
    assert_eq!(f.drain().await.unwrap()["phase"], "blocked");
    assert_eq!(f.signals(), 1);
    let (mut cfg, rev) = workspace_cli::read_config(&f.config).unwrap();
    cfg["extension"] = json!("changed");
    workspace_cli::replace_config(&f.config, &rev, &cfg).unwrap();
    assert!(
        service::test_scope(f.paths(), service::test_drain(&f.config, &f.private, NODE))
            .await
            .is_err()
    );
    assert_eq!(f.signals(), 1);
    f.finish().await;
}
#[tokio::test(flavor = "current_thread")]
async fn foreign_regular_and_dangling_dropins_are_kept() {
    for dangling in [false, true] {
        let mut f = Fixture::new().await;
        std::fs::create_dir_all(f.dropin.parent().unwrap()).unwrap();
        if dangling {
            symlink(f.path("missing"), &f.dropin).unwrap()
        } else {
            write(&f.dropin, b"# Foreign administrator file\n", 0o644)
        }
        assert_eq!(f.drain().await.unwrap()["phase"], "blocked");
        if dangling {
            assert!(f
                .dropin
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink())
        } else {
            assert_eq!(
                std::fs::read(&f.dropin).unwrap(),
                b"# Foreign administrator file\n"
            )
        }
        assert_eq!(f.signals(), 0);
        f.finish().await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn failed_readback_rolls_back_only_the_owned_dropin() {
    let mut f = Fixture::new().await;
    let base = std::fs::read(f.path("base.service")).unwrap();
    f.mutate(|v| v["override"] = json!(f.old));
    assert_eq!(f.drain().await.unwrap()["phase"], "blocked");
    assert!(f.dropin.symlink_metadata().is_err());
    assert_eq!(std::fs::read(f.path("base.service")).unwrap(), base);
    assert_eq!(f.signals(), 0);
    f.finish().await;
}
#[tokio::test(flavor = "current_thread")]
async fn effective_definition_reconciles_readonly_and_preserves_destination() {
    let mut f = Fixture::new().await;
    f.drain().await;
    let path = f.private.join("legacy-service-handover.json");
    let mut record = value(&path);
    let binding = record["binding"].clone();
    record["details"]["definition_verified"] = json!(false);
    save(&path, &record);
    std::fs::write(f.path("manager.calls"), b"").unwrap();
    assert_eq!(f.drain().await.unwrap()["phase"], "draining");
    let record = value(&path);
    assert_eq!(record["binding"], binding);
    let calls = std::fs::read_to_string(f.path("manager.calls")).unwrap();
    assert!(!calls.contains("daemon-reload"));
    assert!(!calls.contains("\"start\""));
    assert_eq!(f.signals(), 1);
    f.finish().await;
    assert!(f.drain().await.is_none());
}
#[tokio::test(flavor = "current_thread")]
async fn activated_profile_starts_current_managed_binary_once() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    let status = f.start(NODE, &revision).await;
    assert_eq!(status["state"], "running");
    assert_eq!(status["generation"].as_str().unwrap().len(), 32);
    let status = f.start(NODE, &revision).await;
    assert_eq!(status["state"], "running");
    assert_eq!(value(&f.path("manager.json"))["starts"], 1);
    f.stop_new().await;
}

#[tokio::test(flavor = "current_thread")]
async fn exact_pending_destination_survives_repeated_draining_checks() {
    let mut f = Fixture::new().await;
    let profile = f.profile(NODE);
    let saved =
        zerus_mobile_connector::workspace::test_reserve_legacy_destination(&f.config, profile)
            .unwrap();
    let revision = workspace_cli::read_config(&f.config).unwrap().1;
    for _ in 0..2 {
        let result = service::test_scope(
            f.paths(),
            zerus_mobile_connector::workspace::manage(
                "check",
                &f.config,
                json!({"expected_revision":revision,"allow_legacy_service_handover":true}),
                false,
            ),
        )
        .await
        .unwrap();
        assert_eq!(result["state"], "migration-draining");
        assert_eq!(value(&f.private.join("pending.json")), saved);
        assert_eq!(result["node_id"], NODE);
    }
    assert_eq!(f.signals(), 1);
    f.finish().await;
}
#[tokio::test(flavor = "current_thread")]
async fn stop_preflight_refuses_unsafe_or_changed_service_before_any_marker() {
    let mut f = Fixture::new().await;
    assert_eq!(f.drain().await.unwrap()["phase"], "draining");
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    let current = f.start(NODE, &revision).await;
    let state = PathBuf::from(value(&f.config)["state_dir"].as_str().unwrap());
    let control = value(&state.join("workspace-control.json"));
    let manager = value(&f.path("manager.json"));
    let record_path = f.private.join("legacy-service-handover.json");
    let record = value(&record_path);
    let input = json!({"expected_revision":revision,"generation":current["generation"]});
    // Every case retains the exact running fixture and must fail before the
    // cooperative Stop marker exists. The fake manager refuses all Stop jobs.
    for fault in [
        "kill-mode",
        "scope",
        "extra-process",
        "extra-subgroup",
        "pid",
        "invocation",
        "missing-record",
        "record-node",
        "control-pid",
        "control-generation",
        "second-read",
        "unreadable-unit",
        "foreign-definition",
    ] {
        save(&f.path("manager.json"), &manager);
        f.replace_private("legacy-service-handover.json", &record);
        save(&state.join("workspace-control.json"), &control);
        let drop_bytes = std::fs::read(&f.dropin).unwrap();
        match fault {
            "kill-mode" => f.mutate(|q| q["properties"]["KillMode"] = json!("control-group")),
            "scope" => f.mutate(|q| q["properties"]["ConsistsOf"] = json!("native.scope")),
            "extra-process" => f.mutate(|q| q["extra_process"] = json!(true)),
            "extra-subgroup" => f.mutate(|q| q["extra_scope"] = json!(true)),
            "pid" => f.mutate(|q| q["properties"]["MainPID"] = json!(f.child.id().to_string())),
            "invocation" => f.mutate(|q| q["invocation"] = json!("replacement")),
            "missing-record" => std::fs::remove_file(&record_path).unwrap(),
            "record-node" => {
                let mut q = record.clone();
                q["managed_node_id"] = json!("replacement");
                f.replace_private("legacy-service-handover.json", &q);
            }
            "control-pid" => {
                let mut q = control.clone();
                q["pid"] = json!(f.child.id());
                save(&state.join("workspace-control.json"), &q);
            }
            "control-generation" => {
                let mut q = control.clone();
                q["generation"] = json!("f".repeat(32));
                save(&state.join("workspace-control.json"), &q);
            }
            "second-read" => {
                f.mutate(|q| q["refuse_on_show"] = json!(q["shows"].as_u64().unwrap() + 2))
            }
            "unreadable-unit" => f.mutate(|q| q["properties"]["LoadState"] = json!("error")),
            "foreign-definition" => {
                std::fs::write(&f.dropin, "[Service]\nKillMode=control-group\n").unwrap()
            }
            _ => unreachable!(),
        }
        assert!(
            service::test_scope(
                f.paths(),
                zerus_mobile_connector::workspace::manage("stop", &f.config, input.clone(), false)
            )
            .await
            .is_err(),
            "{fault}"
        );
        assert!(!state.join("workspace-stop.json").exists(), "{fault}");
        assert!(
            owned_start(control["pid"].as_i64().unwrap() as i32).is_some(),
            "{fault}"
        );
        std::fs::write(&f.dropin, drop_bytes).unwrap();
        let subgroup = f.path("cgroup/fixture/zerus-mobile-connector.service/native.scope");
        if subgroup.exists() {
            std::fs::remove_dir(subgroup).unwrap();
        }
    }
    save(&f.path("manager.json"), &manager);
    f.replace_private("legacy-service-handover.json", &record);
    save(&state.join("workspace-control.json"), &control);
    f.cooperative_stop(&revision, current["generation"].as_str().unwrap())
        .await;
    let calls = std::fs::read_to_string(f.path("manager.calls")).unwrap();
    assert!(calls.lines().all(|line| {
        let args: Vec<String> = serde_json::from_str(line).unwrap();
        matches!(args[1].as_str(), "show" | "daemon-reload" | "start")
    }));
}
#[tokio::test(flavor = "current_thread")]
async fn foreground_stop_needs_exact_lock_owner_generation_and_no_active_service() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    let current = f.start(NODE, &revision).await;
    std::fs::remove_file(f.private.join("legacy-service-handover.json")).unwrap();
    f.mutate(|q| {
        q["group"] = json!("/fixture/session.scope");
        q["properties"] = json!({"LoadState":"not-found","ActiveState":"inactive","MainPID":"0"});
    });
    // Same fixture process, generation and held journal; only the fake
    // supervisor is now known absent. No service job is issued by Stop.
    f.cooperative_stop(&revision, current["generation"].as_str().unwrap())
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn managed_start_stop_start_admits_only_the_observed_clean_generation() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    let first = f.start(NODE, &revision).await;
    f.cooperative_stop(&revision, first["generation"].as_str().unwrap())
        .await;
    let second = f.start(NODE, &revision).await;
    assert_ne!(first["generation"], second["generation"]);
    assert_eq!(value(&f.path("manager.json"))["starts"], 2);
    f.cooperative_stop(&revision, second["generation"].as_str().unwrap())
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn managed_switch_after_update_keeps_old_association_on_failed_cas_and_commits_exact_destination(
) {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    let first = f.start(NODE, &revision).await;
    let prior_image =
        value(&f.private.join("legacy-service-handover.json"))["details"]["desired"].clone();
    let replacement = f.path("replacement");
    std::fs::copy(&f.new, &replacement).unwrap();
    std::fs::rename(&replacement, &f.new).unwrap();
    f.cooperative_stop(&revision, first["generation"].as_str().unwrap())
        .await;
    let next = "00000000-0000-4000-8000-000000000353";
    service::test_scope(
        f.paths(),
        service::test_prepare_switch(&f.private, &f.config, &revision, NODE, next),
    )
    .await
    .unwrap();
    let before = value(&f.private.join("legacy-service-handover.json"));
    assert_ne!(before["details"]["desired"], prior_image);
    assert_eq!(before["managed_node_id"], NODE);
    assert_eq!(before["transfer"]["next_node"], next);
    let (config, _) = workspace_cli::read_config(&f.config).unwrap();
    assert!(workspace_cli::replace_config(&f.config, "wrong-revision", &config).is_err());
    assert_eq!(
        value(&f.private.join("legacy-service-handover.json")),
        before
    );
    assert!(service::test_activated(&f.private, next, "wrong-revision").is_err());
    assert!(service::test_scope(
        f.paths(),
        service::test_start(&f.config, &f.private, next, &revision)
    )
    .await
    .is_err());
    // Exact recovery keeps source admission, pending destination and config intact.
    assert_eq!(value(&f.path("manager.json"))["starts"], 1);
    service::test_scope(
        f.paths(),
        service::test_prepare_switch(&f.private, &f.config, &revision, NODE, next),
    )
    .await
    .unwrap();
    let next_revision = f.activate_as(next);
    let second = f.start(next, &next_revision).await;
    assert_eq!(value(&f.path("manager.json"))["starts"], 2);
    assert_eq!(
        value(&f.private.join("legacy-service-handover.json"))["managed_node_id"],
        next
    );
    assert!(service::test_scope(
        f.paths(),
        service::test_start(&f.config, &f.private, NODE, &next_revision)
    )
    .await
    .is_err());
    f.cooperative_stop(&next_revision, second["generation"].as_str().unwrap())
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn changed_definition_at_final_signal_admission_receives_no_signal() {
    let mut f = Fixture::new().await;
    f.mutate(|v| v["refuse_on_show"] = json!(6));
    assert_eq!(f.drain().await.unwrap()["phase"], "blocked");
    assert_eq!(f.signals(), 0);
    assert_eq!(
        value(&f.private.join("legacy-service-handover.json"))["signal_reserved"],
        true
    );
    f.mutate(|v| v["properties"] = json!({}));
    assert_eq!(f.drain().await.unwrap()["phase"], "draining");
    assert_eq!(f.signals(), 0);
    f.finish().await;
}
#[tokio::test(flavor = "current_thread")]
async fn unknown_start_reservation_never_submits_another_start() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    let p = f.private.join("legacy-service-handover.json");
    let mut record = value(&p);
    record["start_reserved"] = json!(true);
    save(&p, &record);
    assert!(service::test_scope(
        f.paths(),
        service::test_start(&f.config, &f.private, NODE, &revision)
    )
    .await
    .is_err());
    assert_eq!(value(&f.path("manager.json"))["starts"], 0);
}

async fn failed_reservation() -> (Fixture, String, String) {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    assert_eq!(
        value(&f.private.join("legacy-service-handover.json"))["phase"],
        "stopped",
        "fixture must establish the verified managed path before recovery"
    );
    let revision = f.activate();
    let record = f.private.join("legacy-service-handover.json");
    let mut data = value(&record);
    data["start_reserved"] = json!(true);
    save(&record, &data);
    let (cfg, _) = workspace_cli::read_config(&f.config).unwrap();
    let state = PathBuf::from(cfg["state_dir"].as_str().unwrap());
    zerus_mobile_connector::fsguard::create_directory(&state).unwrap();
    write(
        &state.join("workspace-control.json"),
        &serde_json::to_vec(&json!({
            "version":1,"generation":"0123456789abcdef0123456789abcdef","pid":f.child.id()
        }))
        .unwrap(),
        0o600,
    );
    f.mutate(|v| v["properties"] = json!({"Result":"exit-code","ExecMainStatus":"1"}));
    let expected = Store::revision(Some(&std::fs::read(record).unwrap()));
    (f, revision, expected)
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_failed_start_recovery_is_revision_bound_and_audited() {
    let (f, revision, expected) = failed_reservation().await;
    service::test_scope(
        f.paths(),
        service::test_failed_start_recovery(&f.private, &expected),
    )
    .await
    .unwrap();
    let record = value(&f.private.join("legacy-service-handover.json"));
    assert_eq!(record["start_reserved"], false);
    assert!(record["details"]["managed"].is_null());
    let proof = value(
        &f.private
            .join(format!("failed-start-recovery-{expected}.json")),
    );
    assert_eq!(proof["previous_record_revision"], expected);
    assert_eq!(proof["config_revision"], revision);
    assert_eq!(value(&f.path("manager.json"))["starts"], 0);
    assert!(service::test_scope(
        f.paths(),
        service::test_failed_start_recovery(&f.private, &expected)
    )
    .await
    .is_err());
    f.mutate(|v| v["properties"] = json!({}));
    let status = f.start(NODE, &revision).await;
    assert_eq!(value(&f.path("manager.json"))["starts"], 1);
    f.cooperative_stop(&revision, status["generation"].as_str().unwrap())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_failed_start_recovery_refuses_unknown_or_live_state() {
    for case in [
        "revision",
        "clean",
        "activating",
        "live-control",
        "held-journal",
        "observed",
        "definition",
    ] {
        let (f, _, expected) = failed_reservation().await;
        let before = std::fs::read(f.private.join("legacy-service-handover.json")).unwrap();
        let mut supplied = expected.clone();
        let mut held = None;
        match case {
            "revision" => supplied = "0".repeat(64),
            "clean" => f.mutate(|v| v["properties"]["ExecMainStatus"] = json!("0")),
            "activating" => f.mutate(|v| v["properties"]["ActiveState"] = json!("activating")),
            "live-control" => {
                let (cfg, _) = workspace_cli::read_config(&f.config).unwrap();
                let path =
                    Path::new(cfg["state_dir"].as_str().unwrap()).join("workspace-control.json");
                let mut data = value(&path);
                data["pid"] = json!(std::process::id());
                save(&path, &data);
            }
            "observed" => {
                // Malformed or unknown saved proof must fail closed as well.
                let path = f.private.join("legacy-service-handover.json");
                let mut data = value(&path);
                data["details"]["managed"] = json!({});
                save(&path, &data);
                supplied = Store::revision(Some(&std::fs::read(path).unwrap()));
            }
            "held-journal" => {
                let (cfg, _) = workspace_cli::read_config(&f.config).unwrap();
                let lock = Path::new(cfg["state_dir"].as_str().unwrap()).join("connector.lock");
                write(&lock, b"", 0o600);
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(lock)
                    .unwrap();
                zerus_mobile_connector::fsguard::lock_exclusive(&file).unwrap();
                held = Some(file);
            }
            "definition" => f.mutate(|v| v["properties"]["KillMode"] = json!("control-group")),
            _ => unreachable!(),
        }
        assert!(
            service::test_scope(
                f.paths(),
                service::test_failed_start_recovery(&f.private, &supplied)
            )
            .await
            .is_err(),
            "{case}"
        );
        if case != "observed" {
            assert_eq!(
                std::fs::read(f.private.join("legacy-service-handover.json")).unwrap(),
                before
            );
        }
        assert_eq!(value(&f.path("manager.json"))["starts"], 0);
        assert!(!f
            .private
            .join(format!("failed-start-recovery-{supplied}.json"))
            .exists());
        drop(held);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn inactive_remapped_service_never_receives_start() {
    for key in [
        "RootDirectory",
        "RootImage",
        "BindReadOnlyPaths",
        "EnvironmentFiles",
    ] {
        let mut f = Fixture::new().await;
        f.drain().await;
        f.finish().await;
        assert!(f.drain().await.is_none());
        let revision = f.activate();
        f.mutate(|v| v["properties"] = json!({(key):"/synthetic/remapped"}));
        assert!(service::test_scope(
            f.paths(),
            service::test_start(&f.config, &f.private, NODE, &revision)
        )
        .await
        .is_err());
        assert_eq!(value(&f.path("manager.json"))["starts"], 0, "{key}");
    }
}
#[tokio::test(flavor = "current_thread")]
async fn free_legacy_journal_retains_manual_activation_without_service_authority() {
    let mut f = Fixture::new().await;
    f.finish().await;
    f.mutate(|v| v["properties"]["ConsistsOf"] = json!("foreign.scope"));
    assert!(f.drain().await.is_none());
    assert_eq!(
        value(&f.private.join("legacy-service-handover.json"))["phase"],
        "manual"
    );
    assert!(!f.dropin.exists());
    assert_eq!(f.signals(), 0);
    assert_eq!(value(&f.path("manager.json"))["starts"], 0);
    let revision = f.activate();
    assert!(service::test_scope(
        f.paths(),
        service::test_start(&f.config, &f.private, NODE, &revision)
    )
    .await
    .unwrap()
    .is_none());
    let calls = std::fs::read_to_string(f.path("manager.calls")).unwrap_or_default();
    assert!(!calls.contains("daemon-reload") && !calls.contains("\"start\""));
}
#[tokio::test(flavor = "current_thread")]
async fn start_manager_waits_share_one_absolute_deadline() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    f.mutate(|v| v["show_delay"] = json!(6));
    let began = Instant::now();
    assert!(service::test_scope(
        f.paths(),
        service::test_start(&f.config, &f.private, NODE, &revision)
    )
    .await
    .is_err());
    assert!(began.elapsed() < Duration::from_secs(18));
    assert_eq!(value(&f.path("manager.json"))["starts"], 1);
    assert_eq!(
        value(&f.private.join("legacy-service-handover.json"))["start_reserved"],
        true
    );
}

#[tokio::test(flavor = "current_thread")]
async fn source_fifo_is_refused_without_blocking_or_signaling() {
    let mut f = Fixture::new().await;
    let base = f.path("base.service");
    std::fs::remove_file(&base).unwrap();
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &base,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .unwrap();
    let began = Instant::now();
    assert_eq!(f.drain().await.unwrap()["phase"], "blocked");
    assert!(began.elapsed() < Duration::from_secs(3));
    assert_eq!(f.signals(), 0);
    f.finish().await;
}
#[tokio::test(flavor = "current_thread")]
async fn normal_installed_replacement_rebinds_only_after_clean_stop() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    let first = f.start(NODE, &revision).await;
    let previous =
        value(&f.private.join("legacy-service-handover.json"))["details"]["desired"].clone();
    let replacement = f.path("replacement");
    std::fs::copy(&f.new, &replacement).unwrap();
    std::fs::rename(&replacement, &f.new).unwrap();
    assert!(service::test_scope(
        f.paths(),
        service::test_start(&f.config, &f.private, NODE, &revision)
    )
    .await
    .is_err());
    assert_eq!(value(&f.path("manager.json"))["starts"], 1);
    f.cooperative_stop(&revision, first["generation"].as_str().unwrap())
        .await;
    let second = f.start(NODE, &revision).await;
    assert_ne!(
        value(&f.private.join("legacy-service-handover.json"))["details"]["desired"],
        previous
    );
    assert_eq!(value(&f.path("manager.json"))["starts"], 2);
    f.cooperative_stop(&revision, second["generation"].as_str().unwrap())
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn versioned_installed_entrypoint_updates_only_the_owned_dropin() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    let first = f.start(NODE, &revision).await;
    f.cooperative_stop(&revision, first["generation"].as_str().unwrap())
        .await;
    let base = std::fs::read(f.path("base.service")).unwrap();
    let next = f.path("release-next/zerus-mobile-connector");
    std::fs::create_dir(next.parent().unwrap()).unwrap();
    std::fs::copy(&f.new, &next).unwrap();
    f.new = next.clone();
    f.images.push(next);
    let second = f.start(NODE, &revision).await;
    assert_eq!(std::fs::read(f.path("base.service")).unwrap(), base);
    assert!(std::fs::read_to_string(&f.dropin)
        .unwrap()
        .contains(&format!(
            "ExecStart={} --config {}",
            f.new.display(),
            f.config.display()
        )));
    assert_eq!(value(&f.path("manager.json"))["starts"], 2);
    f.cooperative_stop(&revision, second["generation"].as_str().unwrap())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn reserved_restart_cannot_reuse_the_previous_clean_stop() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    let first = f.start(NODE, &revision).await;
    f.cooperative_stop(&revision, first["generation"].as_str().unwrap())
        .await;
    service::test_scope(f.paths(), service::test_reserve_start_only(&f.private))
        .await
        .unwrap();
    assert!(service::test_scope(
        f.paths(),
        service::test_start(&f.config, &f.private, NODE, &revision)
    )
    .await
    .is_err());
    assert_eq!(value(&f.path("manager.json"))["starts"], 1);
}

#[tokio::test(flavor = "current_thread")]
async fn stopped_verified_legacy_unit_still_repairs_managed_autostart() {
    let mut f = Fixture::new().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let record = value(&f.private.join("legacy-service-handover.json"));
    assert_eq!(record["phase"], "stopped");
    assert_eq!(record["details"]["definition_verified"], true);
    assert_eq!(f.signals(), 0);
    let revision = f.activate();
    let status = f.start(NODE, &revision).await;
    assert_eq!(value(&f.path("manager.json"))["starts"], 1);
    f.cooperative_stop(&revision, status["generation"].as_str().unwrap())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn fake_manager_reaps_owned_child_if_registration_fails() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    f.mutate(|v| v["fail_after_spawn"] = json!(true));
    assert!(service::test_scope(
        f.paths(),
        service::test_start(&f.config, &f.private, NODE, &revision)
    )
    .await
    .is_err());
    let intent = value(&f.path("manager.start-intent.json"));
    assert_eq!(intent["pending"], false);
    assert_eq!(intent["outcome"], "reaped-after-error");
    assert!(owned_start(intent["pid"].as_i64().unwrap() as i32).is_none());
    assert_eq!(value(&f.path("manager.json"))["starts"], 0);
}
struct RelayTask(tokio::task::JoinHandle<()>);
impl Drop for RelayTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
#[tokio::test(flavor = "current_thread")]
async fn ordinary_active_check_with_invitation_keeps_explicitly_stopped_service_stopped() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use zerus_mobile_crypto::{Identity, Invitation, Role, SignedMembershipSnapshot};
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let mut profile = f.profile(NODE);
    profile.server_url = format!("http://{}", listener.local_addr().unwrap());
    let identity = Identity::from_record(&profile.identity).unwrap();
    let invitation = Invitation::generate(
        profile.membership.workspace_id.clone(),
        uuid::Uuid::new_v4().to_string(),
        Role::Phone,
        identity.public().clone(),
        zerus_mobile_connector::now() as u64 + 300,
        profile.membership.revision,
        profile.membership.head.clone(),
    )
    .unwrap();
    let bootstrap = SignedMembershipSnapshot::sign(profile.membership.clone(), &identity).unwrap();
    let membership = json!({"changes":[],"membership_revision":profile.membership.revision,"membership_head":profile.membership.head,"next_after":null});
    let revision = f.activate_profile(profile);
    let store = Store::lock(&f.private, false).unwrap();
    store
        .replace(
            "invitations.json",
            "",
            &serde_json::to_vec(
                &json!([{"invitation":invitation,"bootstrap":bootstrap,"approval":null}]),
            )
            .unwrap(),
        )
        .unwrap();
    drop(store);
    let first = f.start(NODE, &revision).await;
    f.cooperative_stop(&revision, first["generation"].as_str().unwrap())
        .await;
    let mut task = RelayTask(tokio::spawn(async move {
        for (route, body) in [
            ("/v2/node/membership?after=0&limit=100", membership),
            ("/v2/node/enrollments?limit=100", json!({"enrollments":[]})),
        ] {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut header = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    assert_eq!(socket.read(&mut byte).await.unwrap(), 1);
                    header.push(byte[0]);
                    assert!(header.len() <= 16384);
                }
            })
            .await
            .unwrap();
            let header = String::from_utf8(header).unwrap();
            assert!(header.starts_with(&format!("GET {route} HTTP/1.1\r\n")));
            let body = serde_json::to_vec(&body).unwrap();
            let head=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    }));
    let result = service::test_scope(
        f.paths(),
        zerus_mobile_connector::workspace::manage(
            "check",
            &f.config,
            json!({"expected_revision":revision}),
            true,
        ),
    )
    .await
    .unwrap();
    assert_eq!(result["state"], "active");
    assert_eq!(result["node_id"], NODE);
    assert_eq!(value(&f.path("manager.json"))["starts"], 1);
    let profile: zerus_mobile_connector::workspace::Profile =
        serde_json::from_value(value(&f.private.join("active.json"))).unwrap();
    assert_eq!(
        zerus_mobile_connector::workspace_lifecycle::status(&profile.state_dir).unwrap()["state"],
        "stopped"
    );
    tokio::time::timeout(Duration::from_secs(5), &mut task.0)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn verified_retired_managed_switch_accepts_missing_and_stale_stop_markers() {
    for stale in [false, true] {
        let mut f = Fixture::new().await;
        f.drain().await;
        f.finish().await;
        assert!(f.drain().await.is_none());
        let revision = f.activate();
        let first = f.start(NODE, &revision).await;
        let state = f.private.join(format!("receipts-{NODE}"));
        let stop = state.join("workspace-stop.json");
        assert!(!stop.exists());
        if stale {
            let generation = first["generation"].as_str().unwrap();
            let stale_generation = format!(
                "{}{}",
                if generation.starts_with('0') {
                    '1'
                } else {
                    '0'
                },
                &generation[1..]
            );
            write(
                &stop,
                &serde_json::to_vec(&json!({"version":1,"generation":stale_generation})).unwrap(),
                0o600,
            );
            assert_ne!(value(&stop)["generation"], first["generation"]);
        }
        f.retire_profile();
        // Test-only emulation of verified retirement's clean exit. It uses the
        // exact retained owned pidfd and writes no lifecycle Stop marker.
        f.stop_new().await;
        let starts = value(&f.path("manager.json"))["starts"].clone();
        let retired_start = service::test_scope(
            f.paths(),
            zerus_mobile_connector::workspace::manage(
                "start",
                &f.config,
                json!({"expected_revision":revision}),
                false,
            ),
        )
        .await
        .unwrap_err();
        assert!(retired_start.to_string().contains("retired"));
        if stale {
            // The same retired proof is needed inside stopped image refresh.
            let replacement = f.path("replacement");
            std::fs::copy(&f.new, &replacement).unwrap();
            std::fs::rename(&replacement, &f.new).unwrap();
        }
        let next = "00000000-0000-4000-8000-000000000353";
        service::test_scope(
            f.paths(),
            service::test_prepare_switch(&f.private, &f.config, &revision, NODE, next),
        )
        .await
        .expect("verified retired managed switch must accept missing or stale Stop marker");
        let record = value(&f.private.join("legacy-service-handover.json"));
        assert_eq!(record["managed_node_id"], NODE);
        assert_eq!(record["transfer"]["previous_revision"], revision);
        assert_eq!(record["transfer"]["previous_node"], NODE);
        assert_eq!(record["transfer"]["next_node"], next);
        assert_eq!(
            record["details"]["managed"]["generation"],
            first["generation"]
        );
        assert_eq!(workspace_cli::read_config(&f.config).unwrap().1, revision);
        assert_eq!(value(&f.path("manager.json"))["starts"], starts);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn active_or_unproven_retirement_without_stop_marker_cannot_switch() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    f.start(NODE, &revision).await;
    f.stop_new().await;
    let active = value(&f.private.join("active.json"));
    let next = "00000000-0000-4000-8000-000000000353";
    for unproven in [false, true] {
        if unproven {
            let mut corrupt = active.clone();
            corrupt["historical_heads"] = json!({});
            f.replace_private("active.json", &corrupt);
        }
        assert!(service::test_scope(
            f.paths(),
            service::test_prepare_switch(&f.private, &f.config, &revision, NODE, next),
        )
        .await
        .is_err());
        assert!(value(&f.private.join("legacy-service-handover.json"))["transfer"].is_null());
        assert_eq!(value(&f.path("manager.json"))["starts"], 1);
    }
    f.replace_private("active.json", &active);
}

#[tokio::test(flavor = "current_thread")]
async fn retired_switch_requires_exact_managed_generation_config_and_node() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    f.start(NODE, &revision).await;
    f.retire_profile();
    f.stop_new().await;
    let state = f.private.join(format!("receipts-{NODE}"));
    let control_path = state.join("workspace-control.json");
    let control = value(&control_path);
    let next = "00000000-0000-4000-8000-000000000353";
    for key in ["generation", "pid"] {
        let mut changed = control.clone();
        changed[key] = if key == "pid" {
            json!(control["pid"].as_i64().unwrap() + 1)
        } else {
            json!("different-generation")
        };
        save(&control_path, &changed);
        assert!(service::test_scope(
            f.paths(),
            service::test_prepare_switch(&f.private, &f.config, &revision, NODE, next),
        )
        .await
        .is_err());
    }
    save(&control_path, &control);
    let (config, _) = workspace_cli::read_config(&f.config).unwrap();
    let mut changed = config.clone();
    changed["server_url"] = json!("https://different.example.test");
    let changed_revision = workspace_cli::replace_config(&f.config, &revision, &changed).unwrap();
    assert!(service::test_scope(
        f.paths(),
        service::test_prepare_switch(&f.private, &f.config, &revision, NODE, next),
    )
    .await
    .is_err());
    assert_eq!(
        workspace_cli::replace_config(&f.config, &changed_revision, &config).unwrap(),
        revision
    );
    assert!(service::test_scope(
        f.paths(),
        service::test_prepare_switch(&f.private, &f.config, &revision, next, NODE),
    )
    .await
    .is_err());
    let record_path = f.private.join("legacy-service-handover.json");
    let record = value(&record_path);
    let mut unknown_start = record.clone();
    unknown_start["details"]["managed"] = Value::Null;
    save(&record_path, &unknown_start);
    assert!(service::test_scope(
        f.paths(),
        service::test_prepare_switch(&f.private, &f.config, &revision, NODE, next),
    )
    .await
    .is_err());
    save(&record_path, &record);
    assert!(value(&record_path)["transfer"].is_null());
    assert_eq!(value(&f.path("manager.json"))["starts"], 1);
}

#[tokio::test(flavor = "current_thread")]
async fn retired_switch_recovers_published_destination_from_exact_archived_history() {
    let mut f = Fixture::new().await;
    f.drain().await;
    f.finish().await;
    assert!(f.drain().await.is_none());
    let revision = f.activate();
    f.start(NODE, &revision).await;
    let previous = value(&f.private.join("active.json"));
    let next = "00000000-0000-4000-8000-000000000353";
    let destination = serde_json::to_value(f.profile(next)).unwrap();
    let pending = json!({
        "version":1,"action":"switch-create","expected_revision":revision,
        "server_url":destination["server_url"],"workspace_name":destination["workspace_name"],
        "computer_name":destination["computer_name"],"node_token":destination["node_token"],
        "identity":destination["identity"],"genesis":null,"candidate":null,"invitation":null,
        "profile":destination,"previous_profile":previous,"retirement":null,
        "legacy_retirement_request":null,"legacy_retirement_status":null,
        "legacy_service_handover":false
    });
    f.replace_private("pending.json", &pending);
    f.retire_profile();
    let retired = value(&f.private.join("active.json"));
    f.stop_new().await;
    service::test_scope(
        f.paths(),
        service::test_prepare_switch(&f.private, &f.config, &revision, NODE, next),
    )
    .await
    .unwrap();
    let archive_name = format!("retired-{NODE}.json");
    f.replace_private(&archive_name, &retired);
    // Production publishes ACTIVE before config CAS. Emulate that exact
    // retained state, including the older unretired Pending previous pin.
    f.replace_private("active.json", &destination);
    let saved_pending = std::fs::read(f.private.join("pending.json")).unwrap();
    let saved_config = std::fs::read(&f.config).unwrap();
    let saved_transfer = value(&f.private.join("legacy-service-handover.json"))["transfer"].clone();
    for corruption in [
        "missing-pin",
        "forked-pin",
        "foreign-workspace",
        "malformed",
    ] {
        let mut changed = retired.clone();
        match corruption {
            "missing-pin" => {
                changed["historical_heads"]
                    .as_object_mut()
                    .unwrap()
                    .remove("0");
                f.replace_private(&archive_name, &changed);
            }
            "forked-pin" => {
                changed["historical_heads"]["0"] = json!(zerus_mobile_crypto::encode(&[37; 32]));
                f.replace_private(&archive_name, &changed);
            }
            "foreign-workspace" => {
                changed["membership"]["workspace_id"] = json!(uuid::Uuid::new_v4().to_string());
                f.replace_private(&archive_name, &changed);
            }
            "malformed" => std::fs::write(f.private.join(&archive_name), b"{").unwrap(),
            _ => unreachable!(),
        }
        assert!(
            service::test_scope(
                f.paths(),
                service::test_prepare_switch(&f.private, &f.config, &revision, NODE, next),
            )
            .await
            .is_err(),
            "{corruption}"
        );
        assert!(std::fs::read(f.private.join("pending.json")).unwrap() == saved_pending);
        assert!(
            value(&f.private.join("legacy-service-handover.json"))["transfer"] == saved_transfer
        );
        f.replace_private(&archive_name, &retired);
    }
    service::test_scope(
        f.paths(),
        service::test_prepare_switch(&f.private, &f.config, &revision, NODE, next),
    )
    .await
    .expect("published destination retry must reuse exact archived retirement proof");
    assert!(std::fs::read(f.private.join("pending.json")).unwrap() == saved_pending);
    assert!(std::fs::read(&f.config).unwrap() == saved_config);
    assert!(value(&f.private.join("active.json")) == destination);
    assert!(value(&f.private.join("legacy-service-handover.json"))["transfer"] == saved_transfer);
    assert_eq!(value(&f.path("manager.json"))["starts"], 1);
    assert_eq!(
        std::fs::read_to_string(f.path("native-calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}
