package main

// Local tests of AppContainer activation, specifications, profile ownership
// and probe classification. A fake profile store stands in for Windows: no
// profile, ACL or contained process is created on a developer computer.

import (
	"encoding/json"
	"errors"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"golang.org/x/sys/windows"
)

var fixtureSource = strings.Repeat("5e", 32)

func passingRecord(binary string) qualificationRecord {
	r := qualificationRecord{Protocol: qualificationProtocol, Evidence: "distributable", Backend: appContainerBackend, BinarySHA256: binary,
		MatrixDigest: matrixDigest(), ConfigIdentity: containerConfigIdentity(), SourceIdentity: fixtureSource,
		OSVersion: "10.0.26100", Runner: "win25 20261124.1"}
	for _, c := range requiredQualificationCases {
		r.Cases = append(r.Cases, qualificationCase{Case: c, Verdict: "pass"})
	}
	return r
}

func TestOnlyACompleteDistributableRecordForThisBinaryActivates(t *testing.T) {
	binary := strings.Repeat("ab", 32)
	host := "10.0.26300"
	if err := checkQualificationRecordOn(passingRecord(binary), binary, fixtureSource, host); err != nil {
		t.Fatal(err)
	}
	// A development build ("unversioned") never activates, whatever the record says.
	if err := checkQualificationRecord(passingRecord(binary), binary); err == nil {
		t.Fatal("an unversioned build accepted a record")
	}
	for name, c := range map[string]struct {
		source, host string
		change       func(*qualificationRecord)
		code         string
	}{
		"other source":         {fixtureSource, host, func(r *qualificationRecord) { r.SourceIdentity = strings.Repeat("6f", 32) }, "qualification-other-source"},
		"missing source":       {fixtureSource, host, func(r *qualificationRecord) { r.SourceIdentity = "" }, "qualification-other-source"},
		"unversioned build":    {"unversioned", host, func(r *qualificationRecord) { r.SourceIdentity = "unversioned" }, "qualification-other-source"},
		"other runner image":   {fixtureSource, host, func(r *qualificationRecord) { r.Runner = "win22 20261124.1" }, "qualification-runner"},
		"missing runner":       {fixtureSource, host, func(r *qualificationRecord) { r.Runner = "" }, "qualification-runner"},
		"runner without build": {fixtureSource, host, func(r *qualificationRecord) { r.Runner = "win25 " }, "qualification-runner"},
		"old qualified OS":     {fixtureSource, host, func(r *qualificationRecord) { r.OSVersion = "10.0.20348" }, "qualification-os"},
		"malformed OS":         {fixtureSource, host, func(r *qualificationRecord) { r.OSVersion = "10.0" }, "qualification-os"},
		"padded OS":            {fixtureSource, host, func(r *qualificationRecord) { r.OSVersion = "10.0.026100" }, "qualification-os"},
		"old host":             {fixtureSource, "10.0.22631", func(*qualificationRecord) {}, "qualification-host-incompatible"},
		"other host version":   {fixtureSource, "11.0.30000", func(*qualificationRecord) {}, "qualification-host-incompatible"},
		"unexpected case":      {fixtureSource, host, func(r *qualificationRecord) { r.Cases = append(r.Cases, qualificationCase{Case: "Q11", Verdict: "pass"}) }, "qualification-unexpected-case"},
		"renamed case":         {fixtureSource, host, func(r *qualificationRecord) { r.Cases[3].Case = "q3" }, "qualification-unexpected-case"},
	} {
		r := passingRecord(binary)
		c.change(&r)
		if err := checkQualificationRecordOn(r, binary, c.source, c.host); err == nil || err.Error() != c.code {
			t.Errorf("%s: %v, want %s", name, err, c.code)
		}
	}
	for name, c := range map[string]struct {
		change func(*qualificationRecord)
		code   string
	}{
		"other binary":     {func(r *qualificationRecord) { r.BinarySHA256 = strings.Repeat("cd", 32) }, "qualification-other-binary"},
		"harness evidence": {func(r *qualificationRecord) { r.Evidence = "harness" }, "qualification-not-distributable"},
		"other matrix":     {func(r *qualificationRecord) { r.MatrixDigest = strings.Repeat("0", 64) }, "qualification-other-matrix"},
		"other config":     {func(r *qualificationRecord) { r.ConfigIdentity = "x" }, "qualification-other-config"},
		"admin service":    {func(r *qualificationRecord) { r.ServiceAdmin = true }, "qualification-admin-service"},
		"protocol":         {func(r *qualificationRecord) { r.Protocol = 2 }, "qualification-protocol"},
		"backend":          {func(r *qualificationRecord) { r.Backend = "windows-local-user-v1" }, "qualification-backend"},
		"missing case":     {func(r *qualificationRecord) { r.Cases = r.Cases[:len(r.Cases)-1] }, "qualification-incomplete"},
		"unsupported case": {func(r *qualificationRecord) { r.Cases[10].Verdict = "unsupported" }, "qualification-incomplete"},
		"failed case":      {func(r *qualificationRecord) { r.Cases[7].Verdict = "fail" }, "qualification-incomplete"},
		"duplicate case":   {func(r *qualificationRecord) { r.Cases = append(r.Cases, r.Cases[0]) }, "qualification-duplicate-case"},
	} {
		r := passingRecord(binary)
		c.change(&r)
		if err := checkQualificationRecordOn(r, binary, fixtureSource, host); err == nil || err.Error() != c.code {
			t.Errorf("%s: %v, want %s", name, err, c.code)
		}
	}
	// Beside this test binary there is no record: activation refuses and the
	// registry stays empty of the backend.
	if err := activateAppContainer(acceptTrust{}); err == nil || err.Error() != "qualification-unavailable" {
		t.Fatalf("activation without a record: %v", err)
	}
	if _, ok := isolationBackends[appContainerBackend]; ok {
		t.Fatal("the backend registered without a record")
	}
	if own, err := ownBinarySHA256(); err != nil || !isHex64(own) {
		t.Fatalf("own binary hash: %q %v", own, err)
	}
}

func TestAStoredSpecCannotNameAnotherContainerOrCapability(t *testing.T) {
	policy := &AccessPolicy{Contexts: map[string]accessContext{acPerson: {"builds", appContainerBackend}}}
	plan, err := planContainer(acInstallation, acHost, JobOwner{Person: acPerson, Project: "proj-shared"}, policy, networkGrant{})
	if err != nil {
		t.Fatal(err)
	}
	spec := specOf(plan)
	back, err := planOf(spec)
	if err != nil || back.Name != plan.Name || !back.SID.Equals(plan.SID) || !back.Capabilities[0].Equals(plan.Capabilities[0]) {
		t.Fatalf("round trip: %+v %v", back, err)
	}
	other, _ := appContainerSID("Zerus.AC.other")
	otherCap, _ := capabilitySID("zerusProjectother")
	for name, change := range map[string]func(*containerSpec){
		"other SID":             func(s *containerSpec) { s.SID = other.String() },
		"other capability SID":  func(s *containerSpec) { s.Capabilities[0] = otherCap.String() },
		"foreign name":          func(s *containerSpec) { s.Name = "Microsoft.Something" },
		"non-project cap":       func(s *containerSpec) { s.CapabilityNames[0] = "internetClient" },
		"missing capability":    func(s *containerSpec) { s.Capabilities = nil },
		"extra capability name": func(s *containerSpec) { s.CapabilityNames = append(s.CapabilityNames, "zerusProjectx") },
	} {
		copy_ := *specOf(plan)
		copy_.Capabilities = append([]string{}, copy_.Capabilities...)
		copy_.CapabilityNames = append([]string{}, copy_.CapabilityNames...)
		change(&copy_)
		if _, err := planOf(&copy_); err == nil {
			t.Errorf("%s accepted", name)
		}
	}
	// The scenario never launches uncontained in a shared executor, and a bad
	// spec refuses before any grant is applied.
	sharedExecution.Store(true)
	defer sharedExecution.Store(false)
	never := func(containedLayout, containerPlan) error { return errors.New("prepare must not run") }
	// Without the service's envelope for this job, nothing launches.
	if _, err := containedLaunchPlan(t.TempDir(), ScenarioConfig{Job: "job-without-envelope", Container: spec}, never); err == nil || err.Error() != "container-envelope-missing" {
		t.Fatalf("no envelope: %v", err)
	}
	executionContainers.Store("", func(s *containerSpec) error {
		if !sameContainer(s, spec) {
			return accessCode("container-envelope-mismatch")
		}
		return nil
	})
	defer executionContainers.Delete("")
	if _, err := containedLaunchPlan(t.TempDir(), ScenarioConfig{}, never); err == nil || err.Error() != "container-missing" {
		t.Fatalf("shared without a container: %v", err)
	}
	bad := *specOf(plan)
	bad.SID = other.String()
	if _, err := containedLaunchPlan(t.TempDir(), ScenarioConfig{Container: &bad}, never); err == nil || err.Error() != "container-envelope-mismatch" {
		t.Fatalf("tampered spec: %v", err)
	}
	var prepared containedLayout
	got, err := containedLaunchPlan(`C:\work`, ScenarioConfig{Container: spec, ArtifactSlot: `C:\slots\a`, ScenarioSlot: `C:\slots\b`},
		func(l containedLayout, p containerPlan) error { prepared = l; return nil })
	if err != nil || got == nil || prepared.Generation != `C:\slots\a\build` || prepared.Runtime != `C:\work\runtime` {
		t.Fatalf("contained plan: %+v %+v %v", got, prepared, err)
	}
	sharedExecution.Store(false)
	if got, err := containedLaunchPlan(`C:\work`, ScenarioConfig{}, never); err != nil || got != nil {
		t.Fatalf("single-person mode: %v %v", got, err)
	}
}

// fakeProfiles records calls and simulates existing profiles.
type fakeProfiles struct {
	existing map[string]bool
	created  []string
	removed  []string
	folder_  string
}

func (f *fakeProfiles) create(name string) (*windows.SID, bool, error) {
	if f.existing[name] {
		return nil, true, nil
	}
	f.existing[name] = true
	f.created = append(f.created, name)
	sid, err := appContainerSID(name)
	return sid, false, err
}
func (f *fakeProfiles) remove(name string) error {
	delete(f.existing, name)
	f.removed = append(f.removed, name)
	return nil
}
func (f *fakeProfiles) folder(*windows.SID) (string, error) { return f.folder_, nil }

func TestProfilesAreUsedOnlyWithTheirOwnOriginRecord(t *testing.T) {
	root := t.TempDir()
	api := &fakeProfiles{existing: map[string]bool{}, folder_: t.TempDir()}
	sid, _ := appContainerSID("Zerus.AC.alpha")
	plan := containerPlan{Name: "Zerus.AC.alpha", SID: sid}
	origin := containerOrigin{Installation: acInstallation, Host: acHost, Person: acPerson, Context: "builds"}
	if err := ensureContainerProfile(api, root, plan, origin); err != nil {
		t.Fatal(err)
	}
	record, err := readOrigin(root, plan.Name)
	if err != nil || record == nil || record.State != "created" || record.SID != sid.String() || record.Person != acPerson {
		t.Fatalf("origin: %+v %v", record, err)
	}
	// Again: the existing profile is ours.
	if err := ensureContainerProfile(api, root, plan, origin); err != nil {
		t.Fatalf("own profile again: %v", err)
	}
	// Another person's origin for the same name is a mismatch.
	if err := ensureContainerProfile(api, root, plan, containerOrigin{Installation: acInstallation, Host: acHost, Person: acOtherPerson, Context: "builds"}); err == nil || err.Error() != "profile-origin-mismatch" {
		t.Fatalf("other person's origin: %v", err)
	}
	// A profile that existed before our record is a collision, also later.
	sidB, _ := appContainerSID("Zerus.AC.beta")
	foreign := containerPlan{Name: "Zerus.AC.beta", SID: sidB}
	api.existing[foreign.Name] = true
	for i := 0; i < 2; i++ {
		if err := ensureContainerProfile(api, root, foreign, origin); err == nil || err.Error() != "profile-collision" {
			t.Fatalf("foreign profile attempt %d: %v", i, err)
		}
	}
	if err := deleteContainerProfile(api, root, foreign); err == nil || err.Error() != "profile-not-owned" {
		t.Fatalf("deleting a collision: %v", err)
	}
	// A crash after creation but before the "created" mark: the record says
	// creating and the profile exists; it is ours and becomes created.
	sidC, _ := appContainerSID("Zerus.AC.gamma")
	crashed := containerPlan{Name: "Zerus.AC.gamma", SID: sidC}
	creating := origin
	creating.Protocol, creating.Name, creating.SID, creating.State, creating.Build = deliveryProtocol, crashed.Name, sidC.String(), "creating", executorSourceIdentity
	if err := durableJSON(originPath(root, crashed.Name), creating); err != nil {
		t.Fatal(err)
	}
	api.existing[crashed.Name] = true
	if err := ensureContainerProfile(api, root, crashed, origin); err != nil {
		t.Fatalf("recovery after a crash: %v", err)
	}
	if r, _ := readOrigin(root, crashed.Name); r.State != "created" {
		t.Fatalf("recovered state %s", r.State)
	}
	// Deletion requires the record and removes both.
	if err := deleteContainerProfile(api, root, plan); err != nil {
		t.Fatal(err)
	}
	if r, _ := readOrigin(root, plan.Name); r != nil || api.existing[plan.Name] {
		t.Fatal("deletion left the record or profile")
	}
}

func TestProfileStorageMustNotBeBroadOrForeign(t *testing.T) {
	sid, _ := appContainerSID("Zerus.AC.alpha")
	other, _ := appContainerSID("Zerus.AC.other")
	plan := containerPlan{Name: "Zerus.AC.alpha", SID: sid}
	for sddl, code := range map[string]string{
		"D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;" + sid.String() + ")":   "",
		"D:P(A;OICI;FA;;;SY)(A;OICI;FR;;;AC)":                     "profile-folder-broad",
		"D:P(A;OICI;FA;;;SY)(A;OICI;FR;;;S-1-15-2-2)":             "profile-folder-broad",
		"D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;" + other.String() + ")": "profile-folder-foreign",
	} {
		sd, err := windows.SecurityDescriptorFromString(sddl)
		if err != nil {
			t.Fatal(err)
		}
		if got := trustCode(profileDACLAllowed(sd, plan)); got != code {
			t.Errorf("%s: %q, want %q", sddl, got, code)
		}
	}
}

func TestPrivateFolderDescriptorsAndExplicitEntries(t *testing.T) {
	sid, _ := appContainerSID("Zerus.AC.alpha")
	capability, _ := capabilitySID("zerusProjectalpha")
	plan := containerPlan{Name: "Zerus.AC.alpha", SID: sid, Capabilities: []*windows.SID{capability}}
	service := currentUserSID(t)
	g, _ := grantFor(roleJobHome, principalPackage)
	sddl, err := privateFolderSDDL(service, g, plan)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := windows.SecurityDescriptorFromString(sddl); err != nil || !strings.HasPrefix(sddl, "D:P") || !strings.Contains(sddl, "S:(ML;OICI;NW;;;LW)") ||
		strings.Contains(sddl, ";AC)") || strings.Count(sddl, "(A;") != 3 {
		t.Fatalf("private folder: %s %v", sddl, err)
	}
	bad := g
	bad.LowLabel = false
	if _, err := privateFolderSDDL(service, bad, plan); err == nil {
		t.Fatal("a writable folder without a Low label")
	}
	sd, _ := windows.SecurityDescriptorFromString("D:AI(A;;FA;;;SY)(A;OICIID;FA;;;BA)(A;OICI;0x1200a9;;;" + capability.String() + ")")
	explicit, found := explicitEntries(sd, "(A;OICI;0x1200a9;;;"+capability.String()+")")
	if !found || strings.Contains(explicit, ";BA)") || !strings.Contains(explicit, ";SY)") {
		t.Fatalf("explicit %s found=%v", explicit, found)
	}
}

// Probe outcomes classify only exact access refusals as denial.
func TestProbeDenialNeedsTheExactCode(t *testing.T) {
	dir := t.TempDir()
	file := filepath.Join(dir, "sentinel.txt")
	if err := os.WriteFile(file, []byte("synthetic"), 0600); err != nil {
		t.Fatal(err)
	}
	if o := runProbeAction(probeAction{"read", file}); !o.OK {
		t.Fatalf("control read: %+v", o)
	}
	// Writing a read-only file is ERROR_ACCESS_DENIED on Windows: denied.
	locked := filepath.Join(dir, "locked.txt")
	if err := os.WriteFile(locked, []byte("synthetic"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(locked, 0444); err != nil {
		t.Fatal(err)
	}
	defer os.Chmod(locked, 0600)
	if o := runProbeAction(probeAction{"write", locked}); !o.denied() || o.Code != errAccessDenied {
		t.Fatalf("access denied: %+v", o)
	}
	// A missing file is not a denial.
	if o := runProbeAction(probeAction{"read", filepath.Join(dir, "absent")}); o.OK || o.denied() || o.Code != 2 {
		t.Fatalf("missing: %+v", o)
	}
	// An absent listener is not a denial.
	l, _ := net.Listen("tcp", "127.0.0.1:0")
	addr := l.Addr().String()
	l.Close()
	if o := runProbeAction(probeAction{"connect", addr}); o.OK || o.denied() {
		t.Fatalf("closed port: %+v", o)
	}
	if o := runProbeAction(probeAction{"nonsense", ""}); o.OK || o.denied() {
		t.Fatalf("unknown action: %+v", o)
	}
	outcomes, err := parseProbeOutcomes([]byte("noise\nPROBE-OUTCOMES [{\"action\":{\"op\":\"read\",\"target\":\"x\"},\"ok\":false,\"code\":5}]\n"))
	if err != nil || !outcomes["read x"].denied() {
		t.Fatalf("parse: %+v %v", outcomes, err)
	}
	if _, err := parseProbeOutcomes([]byte("nothing")); err == nil {
		t.Fatal("parsed a log without outcomes")
	}
	// The qualification command refuses outside the disposable runner.
	if _, err := qualificationEnvironmentGate(); err == nil {
		t.Fatal("qualification gate opened on a developer computer")
	}
}

// fakeContainers is a container backend recording what the executor asked.
type fakeContainers struct {
	calls   int
	origin  containerOrigin
	refuse  error
	planned containerPlan
}

func (f *fakeContainers) Qualified() error { return nil }
func (f *fakeContainers) QualifiedFor(root string, plan containerPlan, origin containerOrigin, trust string) error {
	f.calls++
	f.origin, f.planned = origin, plan
	return f.refuse
}

func TestTheSharedExecutorPlansEachJobsContainerFromTheCurrentPolicy(t *testing.T) {
	f := newSharedFixture(t)
	fake := &fakeContainers{}
	isolationBackends[appContainerBackend] = fake
	defer delete(isolationBackends, appContainerBackend)
	f.s.cfg.Access.Isolation = appContainerBackend
	f.bobBackend = appContainerBackend
	f.revision = 2
	f.writePolicy()
	value := ExecutorSubmission{Owner: &JobOwner{Person: f.person("bob").Person, Device: f.devices["bob-laptop"].fingerprint, Project: "proj-shared", Revision: 2}}
	spec, err := f.s.containerFor(value)
	if err != nil || spec == nil || fake.calls != 1 || fake.origin.Person != f.person("bob").Person || fake.origin.Context != "bob-worker" {
		t.Fatalf("container: %+v %v %+v", spec, err, fake)
	}
	if fake.planned.CapabilityNames[0] == "" || len(fake.planned.Capabilities) != 1 {
		t.Fatalf("capabilities %+v", fake.planned)
	}
	fake.refuse = accessCode("probe-boundary-failed")
	if _, err := f.s.containerFor(value); err == nil || err.Error() != "probe-boundary-failed" {
		t.Fatalf("probe refusal: %v", err)
	}
	// The launch envelope: a coherently substituted spec of another person's
	// container, or of another project, agrees with itself but not with the
	// current policy for this job.
	value.Request.Job = "envelope-job"
	expected, err := f.s.expectedContainer(value)
	if err != nil || !sameContainer(expected, spec) {
		t.Fatalf("expected container: %+v %v", expected, err)
	}
	executionContainers.Store(value.Request.Job, func(s *containerSpec) error {
		want, err := f.s.expectedContainer(value)
		if err != nil {
			return err
		}
		if !sameContainer(s, want) {
			return accessCode("container-envelope-mismatch")
		}
		return nil
	})
	defer executionContainers.Delete(value.Request.Job)
	policy, _ := f.s.currentPolicy()
	alicePlan, err := planContainer(sumBytes([]byte(f.s.root)), f.s.identity, JobOwner{Person: f.person("alice").Person, Project: "proj-shared"},
		&AccessPolicy{Contexts: map[string]accessContext{f.person("alice").Person: {"alice-worker", appContainerBackend}}}, networkGrant{})
	if err != nil {
		t.Fatal(err)
	}
	otherProject, err := planContainer(sumBytes([]byte(f.s.root)), f.s.identity, JobOwner{Person: f.person("bob").Person, Project: "proj-other"}, policy, networkGrant{})
	if err != nil {
		t.Fatal(err)
	}
	for name, substitute := range map[string]*containerSpec{"other person": specOf(alicePlan), "other project": specOf(otherProject)} {
		if _, err := planOf(substitute); err != nil {
			t.Fatalf("%s: the substitute is not even self-consistent: %v", name, err)
		}
		if err := verifyLaunchEnvelope(value.Request.Job, substitute); err == nil || err.Error() != "container-envelope-mismatch" {
			t.Fatalf("%s: %v", name, err)
		}
	}
	if err := verifyLaunchEnvelope(value.Request.Job, spec); err != nil {
		t.Fatalf("the right container: %v", err)
	}
	// Losing membership before launch refuses through the same envelope.
	f.revision = 3
	f.revokedDevices = nil
	f.writePolicy()
	f.revision = 4
	var doc map[string]any
	_ = json.Unmarshal(f.policyBytes(), &doc)
	for _, entry := range doc["projects"].([]any) {
		project := entry.(map[string]any)
		if project["project"] == "proj-shared" {
			project["members"] = []any{map[string]any{"person": f.person("alice").Person, "role": "contributor"}}
		}
	}
	doc["revision"] = 4
	data, _ := json.Marshal(doc)
	if err := os.WriteFile(f.policyPath, data, 0600); err != nil {
		t.Fatal(err)
	}
	if err := verifyLaunchEnvelope(value.Request.Job, spec); err == nil {
		t.Fatal("launched after the owner lost project membership")
	}
	// A client cannot supply a container: the request decoder refuses the field.
	got := f.call("alice-laptop", "POST", "/executor/v1/jobs", []byte(`{"container":{"name":"Zerus.AC.x"}}`), nil)
	if got.status != 400 {
		t.Fatalf("client-supplied container: %d %s", got.status, got.body)
	}
	// alice's context names the synthetic backend: no container plan.
	if _, err := f.s.containerFor(ExecutorSubmission{Owner: &JobOwner{Person: f.person("alice").Person, Project: "proj-shared"}}); err == nil {
		t.Fatal("planned a container for a context of another backend")
	}
}

// Runtime probes run one at a time; concurrent misses for the same key reuse
// the first result, and the cache stays bounded.
func TestRuntimeProbesAreSingleFlightAndBounded(t *testing.T) {
	var calls int32
	release := make(chan struct{})
	b := &appContainerIsolation{binary: strings.Repeat("ab", 32), api: &fakeProfiles{existing: map[string]bool{}, folder_: t.TempDir()}, probes: map[string]time.Time{},
		probe: func(string, containerPlan) error { atomic.AddInt32(&calls, 1); <-release; return nil }}
	sid, _ := appContainerSID("Zerus.AC.alpha")
	plan := containerPlan{Name: "Zerus.AC.alpha", SID: sid}
	origin := containerOrigin{Installation: acInstallation, Host: acHost, Person: acPerson, Context: "builds"}
	root := t.TempDir()
	var wg sync.WaitGroup
	errs := make(chan error, 8)
	for i := 0; i < 8; i++ {
		wg.Add(1)
		go func() { defer wg.Done(); errs <- b.QualifiedFor(root, plan, origin, "trust") }()
	}
	time.Sleep(200 * time.Millisecond)
	close(release)
	wg.Wait()
	close(errs)
	for err := range errs {
		if err != nil {
			t.Fatal(err)
		}
	}
	if calls != 1 {
		t.Fatalf("%d probes ran for one key", calls)
	}
	// A different trust identity is a different key: probed again.
	if err := b.QualifiedFor(root, plan, origin, "other trust"); err != nil || calls != 2 {
		t.Fatalf("trust change: %v, %d probes", err, calls)
	}
	for i := 0; i < maxProbeCache+20; i++ {
		_ = b.QualifiedFor(root, plan, origin, "trust-"+strconv.Itoa(i))
	}
	if len(b.probes) > maxProbeCache {
		t.Fatalf("cache holds %d entries", len(b.probes))
	}
	// A failed probe is not cached.
	b.probe = func(string, containerPlan) error { return accessCode("probe-boundary-failed") }
	if err := b.QualifiedFor(root, plan, origin, "fresh"); err == nil {
		t.Fatal("a failed probe passed")
	}
	if _, cached := b.probes[b.probeKey(plan, "fresh")]; cached {
		t.Fatal("a failed probe was cached")
	}
}
