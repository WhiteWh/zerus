package main

// `hgs-executor appcontainer-qualify CONFIG` is the distributable's own
// qualification of the AppContainer backend. It runs only on a disposable
// GitHub-hosted runner, as a dedicated non-administrator service account,
// with synthetic data; it creates real AppContainer profiles, DACLs and
// labels there and writes a record that trusted packaging may place beside
// this exact binary. Every negative case has a positive control (the service
// itself succeeds) and requires the exact access-denied code; a missing tool,
// listener or file is a failure, never a pass.

import (
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/registry"
)

type qualifyConfig struct {
	Root     string `json:"root"`      // empty folder owned by the service account
	Record   string `json:"record"`    // where the record is written
	ListenIP string `json:"listen_ip"` // a non-loopback address of this runner
	CMake    string `json:"cmake"`
	MSBuild  string `json:"msbuild"`
	// VSInstance is "<installation path>,version=<installation version>",
	// which CMake needs inside a container (no Visual Studio Installer COM).
	VSInstance string `json:"vs_instance"`
	// Toolchain is the MSVC environment (INCLUDE, LIB, LIBPATH, PATH)
	// computed outside the container from vcvars64.bat: an explicit, trusted
	// toolchain input, since batch files cannot run in a new cmd inside it.
	Toolchain map[string]string `json:"toolchain"`
}

// qualificationEnvironmentGate refuses outside the disposable runner and for
// an administrator or elevated service.
func qualificationEnvironmentGate() (bool, error) {
	if os.Getenv("ZERUS_APPCONTAINER_QUALIFICATION") != "1" || os.Getenv("GITHUB_ACTIONS") != "true" ||
		os.Getenv("RUNNER_ENVIRONMENT") != "github-hosted" || !strings.HasPrefix(os.Getenv("ImageOS"), "win25") {
		return false, errors.New("appcontainer-qualify runs only on the disposable windows-2025 runner")
	}
	token, err := serviceToken()
	if err != nil {
		return false, err
	}
	defer token.Close()
	installer, _ := installerSIDs()
	admin := token.IsElevated() || serviceHoldsNone(token, installer, "admin") != nil
	return admin, nil
}

type qualifier struct {
	cfg      qualifyConfig
	plan     containerPlan // person A
	other    containerPlan // person B, another project
	layout   containedLayout
	otherJob string
	cases    []qualificationCase
}

func (q *qualifier) record(name string, err error) {
	verdict, detail := "pass", ""
	if err != nil {
		verdict, detail = "fail", err.Error()
		if strings.HasPrefix(detail, "unsupported:") {
			verdict = "unsupported"
		}
	}
	q.cases = append(q.cases, qualificationCase{name, verdict, detail})
}

func runAppContainerQualification(configPath string) error {
	admin, err := qualificationEnvironmentGate()
	if err != nil {
		return err
	}
	var cfg qualifyConfig
	if err := strictControl(configPath, &cfg); err != nil {
		return err
	}
	binary, err := ownBinarySHA256()
	if err != nil {
		return err
	}
	rec := qualificationRecord{Protocol: qualificationProtocol, Evidence: "distributable", Backend: appContainerBackend, BinarySHA256: binary,
		SourceIdentity: executorSourceIdentity, MatrixDigest: matrixDigest(), ConfigIdentity: containerConfigIdentity(),
		OSVersion: osVersion(), Runner: os.Getenv("ImageOS") + " " + os.Getenv("ImageVersion"), ServiceAdmin: admin}
	q := &qualifier{cfg: cfg}
	if admin {
		q.record("Q0", errors.New("the service account is an administrator or elevated; not representative"))
	} else if err := q.setup(); err != nil {
		q.record("Q0", fmt.Errorf("setup: %w", err))
	} else {
		q.record("Q0", q.q0())
		files, objects, network := q.q1q3q5()
		q.record("Q1", files)
		q.record("Q2", q.q2())
		q.record("Q3", objects)
		q.record("Q4", q.q4())
		q.record("Q5", network)
		q.record("Q6", q.q6())
		q.record("Q7", q.q7())
		q.record("Q8", q.q8())
		q.record("Q9", q.q9())
		q.record("Q10", q.q10())
	}
	rec.Cases = q.cases
	data, _ := json.MarshalIndent(rec, "", "  ")
	if err := os.WriteFile(cfg.Record, data, 0644); err != nil {
		return err
	}
	return checkQualificationRecord(rec, binary)
}

func osVersion() string {
	v := windows.RtlGetVersion()
	return fmt.Sprintf("%d.%d.%d", v.MajorVersion, v.MinorVersion, v.BuildNumber)
}

func (q *qualifier) setup() error {
	// The installation identity is the root's, as in production, so the
	// operator command appcontainer-capability names the same capability.
	parents, rootID, err := holdDirectoryChain(q.cfg.Root)
	closeFiles(parents)
	if err != nil {
		return err
	}
	installation, host := sumBytes([]byte(rootID)), sumBytes([]byte("qualification-host"))
	alice, bob := sumBytes([]byte("synthetic-person-a")), sumBytes([]byte("synthetic-person-b"))
	policy := &AccessPolicy{Contexts: map[string]accessContext{alice: {"builds", appContainerBackend}, bob: {"builds", appContainerBackend}}}
	if q.plan, err = planContainer(installation, host, JobOwner{Person: alice, Project: "proj-shared"}, policy, networkGrant{}); err != nil {
		return err
	}
	if q.other, err = planContainer(installation, host, JobOwner{Person: bob, Project: "proj-other"}, policy, networkGrant{}); err != nil {
		return err
	}
	root := q.cfg.Root
	for _, p := range []struct {
		plan   containerPlan
		person string
	}{{q.plan, alice}, {q.other, bob}} {
		if err := ensureContainerProfile(windowsProfiles{}, root, p.plan, containerOrigin{Installation: installation, Host: host, Person: p.person, Context: "builds"}); err != nil {
			return err
		}
	}
	mk := func(parts ...string) string {
		path := filepath.Join(append([]string{root}, parts...)...)
		_ = os.MkdirAll(path, 0700)
		return path
	}
	q.layout = containedLayout{Root: root, Parent: mk("jobs", "job-a"), Runtime: mk("jobs", "job-a", "runtime"), Outputs: mk("jobs", "job-a", "outputs"),
		Generation: mk("project-slots", "proj-shared", "slot", "build"), Bundle: mk("project-slots", "proj-shared", "bundle", "build")}
	otherParent := mk("jobs", "job-b")
	q.otherJob = mk("jobs", "job-b", "runtime")
	otherOut := mk("jobs", "job-b", "outputs")
	mk("project-slots", "proj-other", "slot", "build")
	mk("project-slots", "proj-other", "bundle", "build")
	for path, text := range map[string]string{
		filepath.Join(root, "identity.key"):                                            "synthetic key sentinel",
		filepath.Join(root, "executor-queue.json"):                                     "synthetic queue sentinel",
		filepath.Join(root, "operator-policy.json"):                                    "synthetic policy sentinel",
		filepath.Join(q.layout.Generation, "input.txt"):                                "synthetic generation",
		filepath.Join(q.layout.Bundle, "scenario.ps1"):                                 "Write-Output 'bundle'",
		filepath.Join(q.layout.Parent, "job-state.json"):                               "synthetic receipt",
		filepath.Join(mk("requests", "job-a"), "run.json"):                             "{\"synthetic\":\"run configuration\"}",
		filepath.Join(mk("profile", ".codex"), "auth.json"):                            "synthetic provider sentinel",
		filepath.Join(mk("broad"), "broad.txt"):                                        "synthetic broad sentinel",
		filepath.Join(mk("project-slots", "proj-other", "slot", "build"), "other.txt"): "synthetic other project",
	} {
		if err := os.WriteFile(path, []byte(text), 0600); err != nil {
			return err
		}
	}
	service, err := serviceSID()
	if err != nil {
		return err
	}
	protected := "D:P(A;OICI;FA;;;" + service.String() + ")(A;OICI;FA;;;SY)"
	if err := applyProtectedTree(root, protected); err != nil {
		return err
	}
	if err := applyProtectedTree(filepath.Join(root, "broad"), protected+"(A;OICI;FR;;;AC)"); err != nil {
		return err
	}
	if err := prepareContainedJob(q.layout, q.plan); err != nil {
		return err
	}
	return prepareContainedJob(containedLayout{Root: root, Parent: otherParent, Runtime: q.otherJob, Outputs: otherOut,
		Generation: filepath.Join(root, "project-slots", "proj-other", "slot", "build"), Bundle: filepath.Join(root, "project-slots", "proj-other", "bundle", "build")}, q.other)
}

// applyProtectedTree sets an inheritable protected DACL on a folder tree
// (runner-only setup of the synthetic world).
func applyProtectedTree(path, sddl string) error {
	sd, err := windows.SecurityDescriptorFromString(sddl)
	if err != nil {
		return err
	}
	dacl, _, _ := sd.DACL()
	p, _ := windows.UTF16PtrFromString(path)
	info := windows.DACL_SECURITY_INFORMATION | windows.PROTECTED_DACL_SECURITY_INFORMATION
	if r, _, _ := procTreeSetNamedSecurityInfo.Call(uintptr(unsafe.Pointer(p)), uintptr(windows.SE_FILE_OBJECT), uintptr(info), 0, 0,
		uintptr(unsafe.Pointer(dacl)), 0, 2, 0, 0, 0); r != 0 {
		return errors.New("protected tree setup failed")
	}
	return nil
}

// expectation is one case of a contained run: the uncontained control must
// succeed, the contained attempt must match want.
type expectation struct {
	action  probeAction
	want    string // "allow", "deny" (exact access-denied), "isolated" (private namespace: not found)
	control bool   // run the uncontained positive control first
}

func (q *qualifier) contained(plan containerPlan, cases []expectation, extra map[string]string) error {
	var actions []probeAction
	var failures []string
	for _, c := range cases {
		if c.control {
			if o := runProbeAction(c.action); !o.OK {
				failures = append(failures, fmt.Sprintf("control %s %s failed: %s", c.action.Op, c.action.Target, o.Detail))
			}
		}
		actions = append(actions, c.action)
	}
	outcomes, err := runContainedProbe(plan, actions, q.layout.Runtime, extra, 120)
	if err != nil {
		return err
	}
	for _, c := range cases {
		o := outcomes[c.action.Op+" "+c.action.Target]
		ok := false
		switch c.want {
		case "allow":
			ok = o.OK
		case "deny":
			ok = o.denied()
		case "isolated":
			ok = !o.OK && (o.Code == 2 || o.Code == errAccessDenied) // ERROR_FILE_NOT_FOUND in the container's namespace
		case "refused":
			// A nested SECURITY_CAPABILITIES launch from inside a container is
			// rejected by the kernel as ERROR_INVALID_PARAMETER (87), with the
			// uncontained control succeeding; access denied also counts.
			ok = !o.OK && (o.Code == errAccessDenied || o.Code == 87)
		case "filtered":
			// AppContainer network isolation drops the connection to a live
			// listener silently. A timeout counts only because the service
			// reaches the same listener before (control) and after (re-check
			// below) the contained attempt in this run.
			ok = !o.OK && (o.Code == wsaAccessDenied || o.Code == wsaTimedOut)
			if ok {
				if after := runProbeAction(c.action); !after.OK {
					ok = false
				}
			}
		}
		if !ok {
			failures = append(failures, fmt.Sprintf("%s %s: want %s, got ok=%v code=%d", c.action.Op, c.action.Target, c.want, o.OK, o.Code))
		}
	}
	if len(failures) != 0 {
		return errors.New(strings.Join(failures, "; "))
	}
	return nil
}

func (q *qualifier) q0() error {
	outcomes, err := runContainedProbe(q.plan, []probeAction{{"env", ""}}, q.layout.Runtime, nil, 60)
	if err != nil {
		return fmt.Errorf("verified contained launch: %w", err)
	}
	if !outcomes["env "].OK {
		return errors.New("contained probe did not run")
	}
	// A foreign profile with a Zerus-looking name is never adopted.
	foreign, _ := appContainerSID("Zerus.AC.foreign-qualification")
	if _, _, err := (windowsProfiles{}).create("Zerus.AC.foreign-qualification"); err != nil {
		return err
	}
	defer (windowsProfiles{}).remove("Zerus.AC.foreign-qualification")
	err = ensureContainerProfile(windowsProfiles{}, q.cfg.Root, containerPlan{Name: "Zerus.AC.foreign-qualification", SID: foreign}, containerOrigin{})
	if err == nil || err.Error() != "profile-collision" {
		return fmt.Errorf("foreign profile: %v", err)
	}
	// A plan whose SID is another container's is refused before resume.
	wrong := q.plan
	wrong.SID = q.other.SID
	native, err := newNativeScenario(256 * MiB)
	if err != nil {
		return err
	}
	defer native.Close()
	self, _ := os.Executable()
	log, _ := os.Create(filepath.Join(q.cfg.Root, "q0.log"))
	defer log.Close()
	env := containedBaseEnvironment(q.layout.Runtime)
	env["ZERUS_CONTAINED_PROBE"] = "[]"
	var token windows.Token
	if err := native.StartContained(q.plan, self, []string{"contained-probe"}, env, q.layout.Runtime, log, func(info processReceipt) error {
		if info.Accepted {
			return windows.OpenProcessToken(native.process, windows.TOKEN_QUERY, &token)
		}
		return nil
	}); err != nil {
		return err
	}
	defer token.Close()
	if err := verifyContainedToken(token, wrong); err == nil || err.Error() != "container-sid-mismatch" {
		return fmt.Errorf("wrong container: %v", err)
	}
	extra := q.plan
	extra.Capabilities = append(append([]*windows.SID{}, q.plan.Capabilities...), q.other.Capabilities...)
	if err := verifyContainedToken(token, extra); err == nil || err.Error() != "container-capabilities-mismatch" {
		return fmt.Errorf("extra capability: %v", err)
	}
	_, _, err = native.Wait(30, nil)
	return err
}

func (q *qualifier) q1q3q5() (error, error, error) {
	root := q.cfg.Root
	files := []expectation{
		{probeAction{"read", filepath.Join(root, "identity.key")}, "deny", true},
		{probeAction{"read", filepath.Join(root, "executor-queue.json")}, "deny", true},
		{probeAction{"names", root}, "allow", true}, // folder names only (cmd.exe batch runs need it)
		{probeAction{"list", root}, "deny", true},   // a full read of the folder stays refused
		{probeAction{"list", q.otherJob}, "deny", true},
		{probeAction{"list", filepath.Join(root, "profile")}, "deny", true},
		{probeAction{"read", filepath.Join(root, "operator-policy.json")}, "deny", true},
		{probeAction{"read", filepath.Join(root, "profile", ".codex", "auth.json")}, "deny", true},
		{probeAction{"read", filepath.Join(q.layout.Parent, "job-state.json")}, "deny", true},
		{probeAction{"read", filepath.Join(q.otherJob, "..", "outputs")}, "deny", false},
		{probeAction{"write", filepath.Join(q.otherJob, "planted.txt")}, "deny", true},
		{probeAction{"read", filepath.Join(root, "project-slots", "proj-other", "slot", "build", "other.txt")}, "deny", true},
		{probeAction{"read", filepath.Join(q.layout.Generation, "input.txt")}, "allow", true},
		{probeAction{"write", filepath.Join(q.layout.Generation, "planted.txt")}, "deny", true},
		{probeAction{"read", filepath.Join(q.layout.Bundle, "scenario.ps1")}, "allow", true},
		{probeAction{"write", filepath.Join(q.layout.Runtime, "home.txt")}, "allow", false},
		{probeAction{"write", filepath.Join(q.layout.Outputs, "evidence.txt")}, "allow", false},
		{probeAction{"write", filepath.Join(q.layout.Parent, "planted.txt")}, "deny", true},
		// The service's launch envelope and receipts cannot be read or rewritten.
		{probeAction{"read", filepath.Join(root, "requests", "job-a", "run.json")}, "deny", true},
		{probeAction{"write", filepath.Join(root, "requests", "job-a", "run.json")}, "deny", true},
		{probeAction{"write", filepath.Join(q.layout.Parent, "job-state.json")}, "deny", true},
		{probeAction{"read", filepath.Join(root, "broad", "broad.txt")}, "allow", true}, // ambient ALL APPLICATION PACKAGES read, documented
	}
	// Clean the control writes so contained writes are judged on their own.
	defer func() {
		for _, p := range []string{filepath.Join(q.otherJob, "planted.txt"), filepath.Join(q.layout.Generation, "planted.txt"), filepath.Join(q.layout.Parent, "planted.txt")} {
			_ = os.Remove(p)
		}
	}()
	other, otherErr := q.startSleeper(q.other)
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err, err, err
	}
	defer listener.Close()
	go acceptAll(listener)
	var lan net.Listener
	if q.cfg.ListenIP != "" {
		if lan, err = net.Listen("tcp", net.JoinHostPort(q.cfg.ListenIP, "0")); err == nil {
			defer lan.Close()
			go acceptAll(lan)
		}
	}
	pipeName := `\\.\pipe\ZerusQualification-` + strconv.Itoa(os.Getpid())
	pipe, err := windows.CreateNamedPipe(windows.StringToUTF16Ptr(pipeName), windows.PIPE_ACCESS_DUPLEX, windows.PIPE_TYPE_BYTE, 255, 512, 512, 0, nil)
	if err != nil {
		return err, err, err
	}
	defer windows.CloseHandle(pipe)
	eventName := `Local\ZerusQualification-` + strconv.Itoa(os.Getpid())
	event, err := windows.CreateEvent(nil, 0, 0, windows.StringToUTF16Ptr(eventName))
	if err != nil {
		return err, err, err
	}
	defer windows.CloseHandle(event)
	key, _, err := registry.CreateKey(registry.CURRENT_USER, `Software\ZerusQualification`, registry.ALL_ACCESS)
	if err != nil {
		return err, err, err
	}
	key.Close()
	defer registry.DeleteKey(registry.CURRENT_USER, `Software\ZerusQualification`)
	scenario, err := newNativeScenario(128 * MiB)
	if err != nil {
		return err, err, err
	}
	defer scenario.Close()
	objects := []expectation{
		{probeAction{"openprocess", strconv.Itoa(os.Getpid())}, "deny", true},
		{probeAction{"openjob", scenario.info.Name}, "isolated", true},
		{probeAction{"pipe", pipeName}, "deny", true},
		{probeAction{"event", eventName}, "isolated", true},
		{probeAction{"regread", `Software\ZerusQualification`}, "deny", true},
		{probeAction{"connect", listener.Addr().String()}, "filtered", true}, // loopback, no exemption
		{probeAction{"spawn", ""}, "allow", true},
		{probeAction{"spawn-breakaway", ""}, "deny", false},
		{probeAction{"spawn-container", q.other.Name}, "refused", true},
		{probeAction{"spawn-capability", q.other.CapabilityNames[0]}, "refused", false},
	}
	if otherErr == nil {
		objects = append(objects, expectation{probeAction{"openprocess", strconv.Itoa(int(other.info.PID))}, "deny", true})
		defer other.Close()
	}
	var network []expectation
	if lan != nil {
		network = append(network, expectation{probeAction{"connect", lan.Addr().String()}, "filtered", true})
	}
	filesErr := q.contained(q.plan, files, nil)
	objectsErr := q.contained(q.plan, objects, nil)
	if otherErr != nil {
		objectsErr = fmt.Errorf("other person's probe: %v; %v", otherErr, objectsErr)
	}
	networkErr := errors.New("no non-loopback listener: network denial not shown")
	if len(network) != 0 {
		networkErr = q.contained(q.plan, network, nil)
	}
	return filesErr, objectsErr, networkErr
}

func acceptAll(l net.Listener) {
	for {
		c, err := l.Accept()
		if err != nil {
			return
		}
		c.Close()
	}
}

// startSleeper runs a contained probe that sleeps, as another person's
// running job.
func (q *qualifier) startSleeper(plan containerPlan) (*nativeScenario, error) {
	native, err := newNativeScenario(128 * MiB)
	if err != nil {
		return nil, err
	}
	self, _ := os.Executable()
	log, err := os.Create(filepath.Join(q.cfg.Root, "sleeper-"+plan.Name[len("Zerus.AC."):]+".log"))
	if err != nil {
		native.Close()
		return nil, err
	}
	spec, _ := json.Marshal([]probeAction{{"sleep", "120"}})
	env := containedBaseEnvironment(q.otherJob)
	env["ZERUS_CONTAINED_PROBE"] = string(spec)
	if err := native.StartContained(plan, self, []string{"contained-probe"}, env, q.otherJob, log, func(processReceipt) error { return nil }); err != nil {
		native.Close()
		return nil, err
	}
	return native, nil
}

func (q *qualifier) q2() error {
	contained := func(sid string) bool {
		return strings.HasPrefix(sid, "S-1-15-2-") || strings.HasPrefix(sid, "S-1-15-3-") || sid == "AC"
	}
	allowedUnder := []string{q.layout.Generation, q.layout.Bundle, q.layout.Parent, filepath.Dir(q.otherJob),
		filepath.Join(q.cfg.Root, "project-slots", "proj-other"), filepath.Join(q.cfg.Root, "broad"), filepath.Join(q.cfg.Root, "appcontainers")}
	return filepath.Walk(q.cfg.Root, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		sd, err := windows.GetNamedSecurityInfo(path, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION)
		if err != nil {
			return err
		}
		allowed := false
		for _, prefix := range allowedUnder {
			if strings.HasPrefix(strings.ToLower(path), strings.ToLower(prefix)) {
				allowed = true
			}
		}
		for _, part := range strings.Split(sd.String(), "(") {
			fields := strings.Split(strings.TrimSuffix(part, ")"), ";")
			// Folder-name listing (no inheritance, no file access) is the
			// matrix's executor-path and project-path entry.
			listingOnly := len(fields) >= 6 && fields[1] == "" && strings.EqualFold(fields[2], fmt.Sprintf("0x%x", rightsListing))
			if len(fields) >= 6 && contained(fields[5]) && !allowed && !listingOnly {
				return fmt.Errorf("%s grants %s", path, fields[5])
			}
			if len(fields) >= 6 && fields[5] == "AC" && !strings.HasPrefix(strings.ToLower(path), strings.ToLower(filepath.Join(q.cfg.Root, "broad"))) {
				return fmt.Errorf("%s grants ALL APPLICATION PACKAGES", path)
			}
		}
		return nil
	})
}

func (q *qualifier) q4() error {
	inherit := &windows.SecurityAttributes{Length: uint32(unsafe.Sizeof(windows.SecurityAttributes{})), InheritHandle: 1}
	sentinel := filepath.Join(q.cfg.Root, "identity.key")
	h, err := windows.CreateFile(windows.StringToUTF16Ptr(sentinel), windows.GENERIC_READ, windows.FILE_SHARE_READ, inherit, windows.OPEN_EXISTING, 0, 0)
	if err != nil {
		return err
	}
	defer windows.CloseHandle(h)
	if found := probeHandleScan("identity.key", os.Getpid(), true); len(found) == 0 {
		return errors.New("positive control: the scan does not see the parent's sentinel")
	}
	env, err := scenarioEnvironment(q.layout.Parent, ScenarioConfig{ArtifactSlot: filepath.Dir(q.layout.Generation), ScenarioSlot: filepath.Dir(q.layout.Bundle), Device: "console"})
	if err != nil {
		return err
	}
	handles := "identity.key|" + strconv.Itoa(os.Getpid())
	outcomes, err := runContainedProbe(q.plan, []probeAction{{"env", ""}, {"handles", handles}}, q.layout.Runtime, env, 60)
	if err != nil {
		return err
	}
	if o := outcomes["handles "+handles]; !o.OK {
		return fmt.Errorf("handles reached the contained child: %s", o.Detail)
	}
	want := map[string]bool{"ZERUS_CONTAINED_PROBE": true}
	for k := range env {
		want[strings.ToUpper(k)] = true
	}
	for _, entry := range strings.Split(outcomes["env "].Detail, "\n") {
		key, _, _ := strings.Cut(entry, "=")
		if key != "" && !want[strings.ToUpper(key)] {
			return fmt.Errorf("extra variable %s", key)
		}
	}
	return nil
}

func (q *qualifier) q6() error {
	native, err := q.startSleeper(q.plan)
	if err != nil {
		return err
	}
	accepted := native.info
	time.Sleep(time.Second)
	if settled, err := namedJobSettled(accepted); settled || err != nil {
		native.Close()
		return fmt.Errorf("a running contained job read as settled: %v %v", settled, err)
	}
	native.Close() // the executor's death closes its Job handle
	for deadline := time.Now().Add(10 * time.Second); ; time.Sleep(100 * time.Millisecond) {
		if settled, err := namedJobSettled(accepted); err == nil && settled {
			break
		}
		if time.Now().After(deadline) {
			return errors.New("the contained tree outlived its Job")
		}
	}
	if settled, err := namedJobSettled(processReceipt{Name: accepted.Name, Issued: true}); settled || err == nil {
		return errors.New("an issued-only receipt read as settled")
	}
	return nil
}

func (q *qualifier) q8() error {
	root := filepath.Join(`C:\`, "ZerusQualification-"+strconv.Itoa(os.Getpid()))
	if err := os.MkdirAll(filepath.Join(root, "operator"), 0700); err != nil {
		return err
	}
	defer os.RemoveAll(root)
	service, err := serviceSID()
	if err != nil {
		return err
	}
	protected := "D:P(A;OICI;FA;;;" + service.String() + ")(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
	if err := applyProtectedTree(root, protected); err != nil {
		return err
	}
	policy := filepath.Join(root, "operator", "policy.json")
	if err := os.WriteFile(policy, []byte("{}"), 0600); err != nil {
		return err
	}
	token, err := serviceToken()
	if err != nil {
		return err
	}
	defer token.Close()
	installer, _ := installerSIDs()
	// The runner's service account stands in as the operator for this
	// descriptor check only; production refuses that pairing.
	// The runner's service account stands in as the descriptor owner; the
	// effective-rights layer uses this token restricted to a synthetic SID
	// (no grant names it), so the planted entries decide the verdict.
	synthetic, _ := windows.StringToSid("S-1-5-21-1000000001-1000000002-1000000003-4102")
	restricted, err := restrictedToken(token, nil, []*windows.SID{synthetic})
	if err != nil {
		return err
	}
	defer restricted.Close()
	identification, err := identificationToken(restricted)
	if err != nil {
		return err
	}
	defer identification.Close()
	trust := &windowsPolicyTrust{append([]*windows.SID{service}, installer...), identification}
	if _, err := readPolicyFile(policy, trust); err != nil {
		if trustCode(err) != "policy-writable-by-service" {
			return fmt.Errorf("protected chain refused: %v", trustDetail(err))
		}
	}
	set := func(path, sddl string) error {
		sd, _ := windows.SecurityDescriptorFromString(sddl)
		dacl, _, _ := sd.DACL()
		return windows.SetNamedSecurityInfo(path, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION|windows.PROTECTED_DACL_SECURITY_INFORMATION, nil, nil, dacl, nil)
	}
	for _, c := range []struct{ path, sddl, label string }{
		{policy, protected + "(A;;FW;;;BU)", "Users write on the file"},
		{filepath.Join(root, "operator"), protected + "(A;;0x40;;;AU)", "Authenticated Users child deletion on the folder"},
		{policy, protected + "(D;;FW;;;WD)(A;;FW;;;WD)", "Everyone write behind a deny"},
	} {
		if err := set(c.path, c.sddl); err != nil {
			return err
		}
		if _, err := readPolicyFile(policy, trust); trustCode(err) != "policy-writable" {
			return fmt.Errorf("%s: %v", c.label, err)
		}
		if err := set(c.path, protected); err != nil {
			return err
		}
	}
	return nil
}

func trustCode(err error) string {
	var refusal *trustError
	if errors.As(err, &refusal) {
		return refusal.code
	}
	if err == nil {
		return ""
	}
	return err.Error()
}

// containedTool runs one real tool in the container with the scenario
// environment plus explicit toolchain folders on PATH.
func (q *qualifier) containedTool(tool string, args []string, path []string) (uint32, string, error) {
	return q.containedToolWith(tool, args, path, nil)
}

func (q *qualifier) containedToolWith(tool string, args []string, path []string, extra map[string]string) (uint32, string, error) {
	native, err := newNativeScenario(2048 * MiB)
	if err != nil {
		return 0, "", err
	}
	defer native.Close()
	logPath := filepath.Join(q.cfg.Root, filepath.Base(tool)+"-"+strconv.FormatInt(time.Now().UnixNano(), 10)+".log")
	log, err := os.Create(logPath)
	if err != nil {
		return 0, "", err
	}
	defer log.Close()
	env, err := scenarioEnvironment(q.layout.Parent, ScenarioConfig{ArtifactSlot: filepath.Dir(q.layout.Generation), ScenarioSlot: filepath.Dir(q.layout.Bundle), Device: "console"})
	if err != nil {
		return 0, "", err
	}
	for k, v := range extra {
		env[k] = v
	}
	env["PATH"] = strings.Join(append(path, env["PATH"]), string(os.PathListSeparator))
	if err := native.StartContained(q.plan, tool, args, env, q.layout.Runtime, log, func(processReceipt) error { return nil }); err != nil {
		return 0, "", err
	}
	code, timedOut, err := native.Wait(600, nil)
	data, _ := os.ReadFile(logPath)
	if timedOut {
		return code, string(data), errors.New("timed out")
	}
	return code, string(data), err
}

func (q *qualifier) q10() error {
	if _, err := exec.LookPath(q.cfg.CMake); err != nil {
		return errors.New("cmake unavailable: not a pass")
	}
	if _, err := os.Stat(q.cfg.MSBuild); err != nil {
		return errors.New("msbuild unavailable: not a pass")
	}
	sources := filepath.Join(q.layout.Generation, "builds")
	if err := os.MkdirAll(sources, 0700); err != nil {
		return err
	}
	files := map[string]string{
		"CMakeLists.txt": "cmake_minimum_required(VERSION 3.20)\nproject(hello C)\nadd_executable(hello main.c)\n",
		"main.c":         "#include <stdio.h>\nint main(void){puts(\"contained-hello\");return 0;}\n",
		"hello.cpp":      "#include <iostream>\n#include <string>\nint main(){std::string s=\"contained-\";std::cout<<s<<\"cpp\"<<std::endl;return 0;}\n",
		"tiny.proj":      "<Project><Target Name=\"Build\"><Message Importance=\"high\" Text=\"contained-msbuild\" /></Target></Project>\n",
		"tiny.ps1":       "$value = 'contained-' + (1+1)\n$value\n",
	}
	for name, text := range files {
		if err := os.WriteFile(filepath.Join(sources, name), []byte(text), 0600); err != nil {
			return err
		}
	}
	var failures []string
	check := func(label, want string, code uint32, log string, err error) bool {
		if err != nil || code != 0 || !strings.Contains(log, want) {
			tail := log
			if len(tail) > 4000 {
				tail = tail[len(tail)-4000:]
			}
			failures = append(failures, fmt.Sprintf("%s: code %d err %v log %q", label, code, err, tail))
			return false
		}
		return true
	}
	powershell := filepath.Join(os.Getenv("SystemRoot"), "System32", "WindowsPowerShell", "v1.0", "powershell.exe")
	code, log, err := q.containedTool(powershell, []string{"-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", filepath.Join(sources, "tiny.ps1")}, nil)
	check("PowerShell", "contained-2", code, log, err)
	code, log, err = q.containedTool(q.cfg.MSBuild, []string{filepath.Join(sources, "tiny.proj"), "-nologo", "-noAutoResponse"}, []string{filepath.Dir(q.cfg.MSBuild)})
	check("MSBuild", "contained-msbuild", code, log, err)
	// A real C++ build: the MSVC environment computed outside the container
	// (the workflow runs vcvars64.bat and passes INCLUDE, LIB, LIBPATH and
	// PATH), CMake with NMake and cl; no batch file runs in the container.
	// Ninja is not used: it creates a global named pipe, which a container
	// may not.
	system, _ := systemDirectory()
	cmd := filepath.Join(system, "cmd.exe")
	build := filepath.Join(q.layout.Runtime, "nmake-build")
	// Windows environment names are case-insensitive (`set` prints "Path").
	upper := map[string]string{}
	for k, v := range q.cfg.Toolchain {
		upper[strings.ToUpper(k)] = v
	}
	toolchain := map[string]string{}
	for _, key := range []string{"INCLUDE", "LIB", "LIBPATH"} {
		toolchain[key] = upper[key]
	}
	toolPath := strings.Split(upper["PATH"], string(os.PathListSeparator))
	if toolchain["INCLUDE"] == "" || toolchain["LIB"] == "" || len(toolPath) < 2 {
		failures = append(failures, "MSVC toolchain environment not provided by the workflow")
	} else {
		// Required: MSVC compiles and links C++ inside the container (cl
		// starts link itself; no debug database server, no pipes).
		cl := ""
		for _, dir := range toolPath {
			if candidate := filepath.Join(dir, "cl.exe"); fileExists(candidate) {
				cl = candidate
				break
			}
		}
		if cl == "" {
			failures = append(failures, "cl.exe not found on the toolchain PATH")
		} else {
			code, log, err = q.containedToolWith(cl, []string{"/nologo", "/EHsc", "/O2", filepath.Join(sources, "hello.cpp"), "/Fe" + filepath.Join(q.layout.Runtime, "hello-cpp.exe")}, toolPath, toolchain)
			if check("C++ compile and link (MSVC)", "", code, log, err) {
				code, log, err = q.containedTool(filepath.Join(q.layout.Runtime, "hello-cpp.exe"), nil, nil)
				check("built C++ program", "contained-cpp", code, log, err)
			}
		}
		// Recorded, not required: CMake's generators wrap the compiler in
		// `cmake -E` helpers (and Ninja) that create global named pipes,
		// which an AppContainer may not.
		cmCode, cmLog, cmErr := q.containedToolWith(q.cfg.CMake, []string{"-S", sources, "-B", build, "-G", "NMake Makefiles", "-DCMAKE_BUILD_TYPE=Release",
			"-DCMAKE_C_COMPILER=cl.exe"}, append([]string{filepath.Dir(q.cfg.CMake)}, toolPath...), toolchain)
		if cmErr == nil && cmCode == 0 {
			fmt.Fprintln(os.Stderr, "note: CMake (NMake) also configures inside the container")
		} else {
			fmt.Fprintf(os.Stderr, "note: CMake generators unsupported inside the container: code %d %v %.300q\n", cmCode, cmErr, cmLog)
		}
	}
	// MSBuild C++ (vcxproj) build events: a known AppContainer limitation,
	// recorded for the operator and not required.
	vsBuild := filepath.Join(q.layout.Runtime, "vs-build")
	vsCode, vsLog, vsErr := q.containedTool(q.cfg.CMake, []string{"-S", sources, "-B", vsBuild, "-G", "Visual Studio 18 2026", "-A", "x64",
		"-DCMAKE_GENERATOR_INSTANCE=" + q.cfg.VSInstance}, []string{filepath.Dir(q.cfg.CMake), filepath.Dir(q.cfg.MSBuild)})
	if vsErr == nil && vsCode == 0 {
		fmt.Fprintln(os.Stderr, "note: MSBuild C++ projects also configure inside the container")
	} else {
		fmt.Fprintf(os.Stderr, "note: MSBuild C++ projects unsupported inside the container (build events): code %d %v %.200q\n", vsCode, vsErr, vsLog)
	}
	if len(failures) != 0 {
		// Diagnostics for the record (they do not change the verdict): which
		// part of a batch-file run is refused inside the container.
		batch := filepath.Join(q.layout.Runtime, "diag.cmd")
		_ = os.WriteFile(batch, []byte("@echo diag-batch-runtime\r\n"), 0600)
		diag := func(label string, tool string, args []string) {
			code, log, err := q.containedTool(tool, args, nil)
			if len(log) > 500 {
				log = log[len(log)-500:]
			}
			failures = append(failures, fmt.Sprintf("diag %s: code %d err %v log %q", label, code, err, log))
		}
		diag("cmd direct", cmd, []string{"/d", "/c", "echo diag-direct"})
		diag("batch in runtime", cmd, []string{"/d", "/q", "/c", batch})
		diag("batch in container temp", cmd, []string{"/d", "/c", "echo @echo diag-ac-temp> %TEMP%\\d3.cmd && call %TEMP%\\d3.cmd && echo temp=%TEMP%"})
		// Experiment only: list (names, no file contents) on the runtime's
		// ancestors for this package, then the runtime batch again.
		listing := containedGrant{roleJobParent, principalPackage, windows.FILE_LIST_DIRECTORY | rightsTraverse, false, false}
		for _, dir := range []string{q.cfg.Root, filepath.Join(q.cfg.Root, "jobs"), q.layout.Parent} {
			if err := addExplicitEntry(dir, listing, q.plan, false); err != nil {
				failures = append(failures, "diag list grant on "+dir+": "+err.Error())
			}
		}
		diag("batch in runtime with ancestor listing", cmd, []string{"/d", "/q", "/c", batch})
		bundleBatch := filepath.Join(q.layout.Bundle, "diag-bundle.cmd")
		_ = os.WriteFile(bundleBatch, []byte("@echo diag-batch-bundle\r\n"), 0600)
		diag("batch in bundle", cmd, []string{"/d", "/q", "/c", bundleBatch})
		diag("batch written by the container in runtime", cmd, []string{"/d", "/c", "echo @echo diag-own> own.cmd && call own.cmd"})
		diag("service batch via call", cmd, []string{"/d", "/c", "call " + batch})
		// Hypothesis: cmd.exe reads the DisableCMD policy before running a
		// batch file and the container cannot read the user's policy key.
		regDiag := func(label string) {
			outcomes, err := runContainedProbe(q.plan, []probeAction{{"regread", `Software\Policies\Microsoft\Windows\System`}, {"regread", `Software\Policies`}}, q.layout.Runtime, nil, 60)
			if err != nil {
				failures = append(failures, "diag "+label+": "+err.Error())
				return
			}
			for k, o := range outcomes {
				failures = append(failures, fmt.Sprintf("diag %s %s: ok=%v code=%d", label, k, o.OK, o.Code))
			}
		}
		regDiag("policy key before grant")
		if err := grantPolicyKeyRead(q.plan); err != nil {
			failures = append(failures, "diag policy key grant: "+err.Error())
		}
		regDiag("policy key after grant")
		diag("cmd /c batch after policy grant", cmd, []string{"/d", "/q", "/c", batch})
		diag("container batch via cmd /c", cmd, []string{"/d", "/c", "echo @echo diag-own2> own2.cmd && " + cmd + " /d /q /c " + filepath.Join(q.layout.Runtime, "own2.cmd")})
		diag("service batch read", cmd, []string{"/d", "/c", "type " + batch})
		diag("icacls service batch", filepath.Join(system, "icacls.exe"), []string{batch})
		diag("icacls container batch", filepath.Join(system, "icacls.exe"), []string{filepath.Join(q.layout.Runtime, "own2.cmd")})
		return errors.New("unsupported: " + strings.Join(failures, "; "))
	}
	return nil
}

// grantPolicyKeyRead gives the plan's package SID read on the user's
// HKCU\Software\Policies\Microsoft\Windows\System key (created empty if
// missing), the key cmd.exe consults before running a batch file.
// Diagnostic for the runner only.
func grantPolicyKeyRead(plan containerPlan) error {
	for _, path := range []string{`Software\Policies`, `Software\Policies\Microsoft`, `Software\Policies\Microsoft\Windows`, `Software\Policies\Microsoft\Windows\System`} {
		key, _, err := registry.CreateKey(registry.CURRENT_USER, path, registry.READ|windows.WRITE_DAC|windows.READ_CONTROL)
		if err != nil {
			return fmt.Errorf("%s: %w", path, err)
		}
		sd, err := windows.GetSecurityInfo(windows.Handle(key), windows.SE_REGISTRY_KEY, windows.DACL_SECURITY_INFORMATION)
		if err != nil {
			key.Close()
			return err
		}
		explicit, _ := explicitEntries(sd, "")
		next, err := windows.SecurityDescriptorFromString("D:" + explicit + "(A;CI;KR;;;" + plan.SID.String() + ")")
		if err != nil {
			key.Close()
			return err
		}
		dacl, _, _ := next.DACL()
		err = windows.SetSecurityInfo(windows.Handle(key), windows.SE_REGISTRY_KEY, windows.DACL_SECURITY_INFORMATION|windows.UNPROTECTED_DACL_SECURITY_INFORMATION, nil, nil, dacl, nil)
		key.Close()
		if err != nil {
			return fmt.Errorf("%s: %w", path, err)
		}
	}
	return nil
}

func fileExists(path string) bool {
	info, err := os.Stat(path)
	return err == nil && !info.IsDir()
}
