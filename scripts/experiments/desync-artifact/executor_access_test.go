package main

// Two-person integration tests for the multi-person executor (milestone 1).
// They register a synthetic isolation backend: it qualifies admission in these
// tests only and is not a security boundary. No production build compiles this
// file, and no configuration or environment value selects the backend.

import (
	"golang.org/x/sys/windows"

	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"io"
	"math/big"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"
)

type syntheticIsolation struct{}

func (syntheticIsolation) Qualified() error { return nil }

func init() { isolationBackends["synthetic-test"] = syntheticIsolation{} }

type fixturePerson struct {
	Name, Seed, Person string
	PublicKey          string `json:"public"`
}

type testDevice struct {
	cert        *x509.Certificate
	pair        tls.Certificate
	fingerprint string
}

func newTestDevice(t *testing.T) testDevice {
	t.Helper()
	pub, key, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	template := &x509.Certificate{SerialNumber: big.NewInt(time.Now().UnixNano()), Subject: pkix.Name{CommonName: "synthetic device"},
		NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(time.Hour),
		KeyUsage: x509.KeyUsageDigitalSignature | x509.KeyUsageCertSign, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageClientAuth},
		BasicConstraintsValid: true, IsCA: true}
	der, err := x509.CreateCertificate(rand.Reader, template, template, pub, key)
	if err != nil {
		t.Fatal(err)
	}
	cert, _ := x509.ParseCertificate(der)
	sum := sha256.Sum256(der)
	return testDevice{cert, tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}, hex.EncodeToString(sum[:])}
}

// sharedFixture is one executor root with alice and bob (contributors of
// proj-shared; alice alone in proj-alice) and carol, an operator without a
// project or context.
type sharedFixture struct {
	t                *testing.T
	s                *executorService
	host, policyPath string
	persons          map[string]fixturePerson
	devices          map[string]testDevice
	revision         uint64
	revokedDevices   []string
	revokedPersons   []string
	bobBackend       string
	dataPeer         string
	now              time.Time
}

func newSharedFixture(t *testing.T) *sharedFixture {
	t.Helper()
	var keys struct{ Persons []fixturePerson }
	hostAccessFixture(t, "synthetic-keys.json", &keys)
	f := &sharedFixture{t: t, persons: map[string]fixturePerson{}, devices: map[string]testDevice{}, revision: 1, bobBackend: "synthetic-test",
		now: time.Now()}
	for _, p := range keys.Persons {
		f.persons[p.Name] = p
	}
	for _, name := range []string{"alice-laptop", "alice-phone", "bob-laptop", "carol-console", "stranger"} {
		f.devices[name] = newTestDevice(t)
	}
	base := t.TempDir()
	root := filepath.Join(base, "executor")
	operator := filepath.Join(base, "operator")
	for _, dir := range []string{root, operator} {
		if err := os.Mkdir(dir, 0700); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(filepath.Join(operator, "identity-init.json"), []byte("{}"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := initializeIdentity(filepath.Join(operator, "identity-init.json")); err != nil {
		t.Fatal(err)
	}
	_, host, err := loadIdentity(filepath.Join(operator, "identity"))
	if err != nil {
		t.Fatal(err)
	}
	f.host = host
	f.policyPath = filepath.Join(operator, "access-policy.json")
	f.dataPeer = strings.Repeat("d", 64)
	f.writePolicy()
	parents, rootIdentity, err := holdDirectoryChain(root)
	closeFiles(parents)
	if err != nil {
		t.Fatal(err)
	}
	cfg := ExecutorConfig{Root: root, Peer: PeerConfig{Identity: filepath.Join(operator, "identity"), Listen: "127.0.0.1:0"}, DataPeers: []string{f.dataPeer},
		Access: &AccessConfig{Policy: f.policyPath, Operator: syntheticOperator, Isolation: "synthetic-test", PersonJobs: 2}}
	if err := validateAccessConfig(cfg); err != nil {
		t.Fatal(err)
	}
	f.s = &executorService{cfg: cfg, identity: host, root: rootIdentity, wake: make(chan struct{}, 1), stop: make(chan struct{}), settled: make(chan struct{}),
		queue: ExecutorQueue{deliveryProtocol, rootIdentity, []string{}}}
	f.s.access.now = func() time.Time { return f.now }
	f.s.access.trust = acceptTrust{}
	sharedExecution.Store(true)
	t.Cleanup(func() { sharedExecution.Store(false) })
	if _, err := f.s.currentPolicy(); err != nil {
		t.Fatal(err)
	}
	if err := f.s.prepareAccessRoot(); err != nil {
		t.Fatal(err)
	}
	return f
}

// service is a fresh executor service on the fixture's root (a restart).
func (f *sharedFixture) service(cfg ExecutorConfig) *executorService {
	s := &executorService{cfg: cfg, identity: f.s.identity, root: f.s.root, wake: make(chan struct{}, 1), stop: make(chan struct{}), settled: make(chan struct{}),
		queue: ExecutorQueue{deliveryProtocol, f.s.root, []string{}}}
	s.access.now = func() time.Time { return f.now }
	s.access.trust = acceptTrust{}
	return s
}

func (f *sharedFixture) person(name string) fixturePerson { return f.persons[name] }

func (f *sharedFixture) policyBytes() []byte {
	type m = map[string]any
	key := func(name string) ed25519.PrivateKey {
		seed, _ := hex.DecodeString(f.persons[name].Seed)
		return ed25519.NewKeyFromSeed(seed)
	}
	now := uint64(time.Now().Unix())
	enroll := func(owner, device string) m {
		d := f.devices[device].fingerprint
		person := f.persons[owner].Person
		issued, expires := now-3600, now+86400
		signature := ed25519.Sign(key(owner), enrollmentStatement(f.host, person, d, issued, expires))
		return m{"device": d, "person": person, "issued": issued, "expires": expires, "signature": base64.RawURLEncoding.EncodeToString(signature)}
	}
	row := func(name string) m {
		p := f.persons[name]
		return m{"person": p.Person, "public_key": p.PublicKey, "label": "Synthetic " + name}
	}
	alice, bob, carol := f.persons["alice"].Person, f.persons["bob"].Person, f.persons["carol"].Person
	policy := m{
		"contract": hostAccessContract, "host": f.host, "revision": f.revision,
		"persons": []m{row("alice"), row("bob"), row("carol")},
		"devices": []m{enroll("alice", "alice-laptop"), enroll("alice", "alice-phone"), enroll("bob", "bob-laptop"), enroll("carol", "carol-console")},
		"projects": []m{
			{"project": "proj-shared", "members": []m{{"person": alice, "role": "contributor"}, {"person": bob, "role": "contributor"}}},
			{"project": "proj-alice", "members": []m{{"person": alice, "role": "contributor"}}},
		},
		"operators":       []m{{"person": carol, "rights": []string{"resource-cancel"}}},
		"contexts":        []m{{"person": alice, "context": "alice-worker", "backend": "synthetic-test"}, {"person": bob, "context": "bob-worker", "backend": f.bobBackend}},
		"revoked_devices": append([]string{}, f.revokedDevices...),
		"revoked_persons": append([]string{}, f.revokedPersons...),
	}
	data, _ := json.Marshal(policy)
	return data
}

func (f *sharedFixture) writePolicy() {
	if err := os.WriteFile(f.policyPath, f.policyBytes(), 0600); err != nil {
		f.t.Fatal(err)
	}
}

func hex64(seed string) string {
	sum := sha256.Sum256([]byte(seed))
	return hex.EncodeToString(sum[:])
}

func (f *sharedFixture) request(job, project string) ExecutorRequest {
	offer := func(name string) TreeOffer {
		return TreeOffer{Protocol: deliveryProtocol, ID: hex64(name), CatalogBytes: 1, Endpoint: "https://127.0.0.1:9", Token: hex64("token " + name),
			Session: hex64("session")[:32], Peer: f.dataPeer}
	}
	return ExecutorRequest{Protocol: deliveryProtocol, Job: job, Slot: "slot-a", Device: "host", Artifact: offer("artifact " + job), Scenario: offer("scenario"),
		Parameters: map[string]json.RawMessage{}, Project: project}
}

func newJob(name string) string { return hex64("job " + name)[:32] }

type answer struct {
	status int
	body   string
}

// call sends one request as device, bound to the current revision and a
// deadline unless headers override them.
func (f *sharedFixture) call(device, method, path string, body any, headers map[string]string) answer {
	f.t.Helper()
	var reader io.Reader
	if body != nil {
		if raw, ok := body.(string); ok {
			reader = strings.NewReader(raw)
		} else {
			data, _ := json.Marshal(body)
			reader = bytes.NewReader(data)
		}
	}
	r := httptest.NewRequest(method, path, reader)
	r.TLS = &tls.ConnectionState{PeerCertificates: []*x509.Certificate{f.devices[device].cert}}
	r.Header.Set("Zerus-Policy-Revision", strconv.FormatUint(f.revision, 10))
	r.Header.Set("Zerus-Request-Deadline", strconv.FormatInt(f.now.UnixMilli()+60000, 10))
	for k, v := range headers {
		if v == "" {
			r.Header.Del(k)
		} else {
			r.Header.Set(k, v)
		}
	}
	w := httptest.NewRecorder()
	f.s.ServeHTTP(w, r)
	return answer{w.Code, w.Body.String()}
}

func (f *sharedFixture) submit(device, job, project string) answer {
	return f.call(device, "POST", "/executor/v1/jobs", f.request(job, project), nil)
}

func expect(t *testing.T, got answer, status int, what string) {
	t.Helper()
	if got.status != status {
		t.Fatalf("%s: status %d (%s), want %d", what, got.status, strings.TrimSpace(got.body), status)
	}
}

func TestTwoPeopleSeeAndControlOnlyTheirOwnJobs(t *testing.T) {
	f := newSharedFixture(t)
	jobA, jobB := newJob("a"), newJob("b")
	expect(t, f.submit("alice-laptop", jobA, "proj-shared"), 200, "alice submits")
	expect(t, f.submit("bob-laptop", jobB, "proj-shared"), 200, "bob submits")

	list := f.call("alice-laptop", "GET", "/executor/v1/jobs", nil, nil)
	if !strings.Contains(list.body, jobA) || strings.Contains(list.body, jobB) {
		t.Fatalf("alice's list: %s", list.body)
	}
	list = f.call("bob-laptop", "GET", "/executor/v1/jobs", nil, nil)
	if !strings.Contains(list.body, jobB) || strings.Contains(list.body, jobA) || strings.Contains(list.body, f.person("alice").Person) {
		t.Fatalf("bob's list: %s", list.body)
	}
	// Every per-job route answers bob as if alice's job did not exist.
	release := map[string]any{"request_id": newJob("r")[:32], "request_sha256": strings.Repeat("0", 64)}
	for _, route := range []struct {
		method, path string
		body         any
	}{
		{"GET", "/executor/v1/jobs/" + jobA, nil},
		{"GET", "/executor/v1/jobs/" + jobA + "/log", nil},
		{"GET", "/executor/v1/jobs/" + jobA + "/evidence/result", nil},
		{"POST", "/executor/v1/jobs/" + jobA + "/release", release},
		{"PUT", "/executor/v1/jobs/" + jobA + "/publication", f.request(jobA, "proj-shared")},
		{"POST", "/executor/v1/jobs/" + jobA + "/observation", map[string]any{}},
		{"POST", "/executor/v1/jobs/" + jobA + "/acknowledgement", map[string]any{}},
		{"POST", "/executor/v1/jobs/" + jobA + "/cancel", map[string]any{}},
	} {
		got := f.call("bob-laptop", route.method, route.path, route.body, nil)
		if got.status != 404 || strings.Contains(got.body, f.person("alice").Person) {
			t.Fatalf("bob %s %s: %d %s", route.method, route.path, got.status, got.body)
		}
	}
	expect(t, f.call("alice-laptop", "GET", "/executor/v1/jobs/"+jobA, nil, nil), 200, "alice reads her job")
	expect(t, f.call("alice-phone", "GET", "/executor/v1/jobs/"+jobA, nil, nil), 200, "alice's second device reads it")
}

func TestAJobIDAnswersOnlyItsOwner(t *testing.T) {
	f := newSharedFixture(t)
	job := newJob("collide")
	first := f.submit("alice-laptop", job, "proj-shared")
	expect(t, first, 200, "alice submits")
	// The same request by another person, or by alice for another project,
	// is refused without the receipt.
	got := f.submit("bob-laptop", job, "proj-shared")
	if got.status != 409 || strings.Contains(got.body, f.person("alice").Person) || strings.Contains(got.body, "queued") {
		t.Fatalf("bob reused alice's job ID: %d %s", got.status, got.body)
	}
	expect(t, f.submit("alice-laptop", job, "proj-alice"), 409, "alice reuses the ID for another project")
	// The owner's exact repeat returns the original receipt (semantic dedup).
	again := f.submit("alice-laptop", job, "proj-shared")
	expect(t, again, 200, "alice repeats")
	var a, b ExecutorSubmission
	_ = json.Unmarshal([]byte(first.body), &a)
	_ = json.Unmarshal([]byte(again.body), &b)
	if a.Created != b.Created || b.Owner == nil || b.Owner.Person != f.person("alice").Person {
		t.Fatalf("repeat changed the receipt: %+v %+v", a, b)
	}
}

func TestRequestFieldsCannotClaimAnOwner(t *testing.T) {
	f := newSharedFixture(t)
	request := f.request(newJob("forged"), "proj-shared")
	data, _ := json.Marshal(request)
	for _, field := range []string{`"owner":{"person":"` + f.person("bob").Person + `"}`, `"person":"` + f.person("bob").Person + `"`, `"role":"contributor"`} {
		forged := strings.Replace(string(data), "{", "{"+field+",", 1)
		expect(t, f.call("alice-laptop", "POST", "/executor/v1/jobs", forged, nil), 400, "forged "+field)
	}
	// A project the caller does not contribute to is refused before anything is written.
	expect(t, f.submit("bob-laptop", newJob("x"), "proj-alice"), 403, "bob submits to alice's project")
	expect(t, f.submit("carol-console", newJob("y"), "proj-shared"), 403, "operator submits")
	if _, err := os.Stat(filepath.Join(f.s.cfg.Root, "submissions")); !os.IsNotExist(err) {
		t.Fatal("a refused submission wrote state")
	}
}

func TestMutationsAreBoundToTheCurrentRevisionAndADeadline(t *testing.T) {
	f := newSharedFixture(t)
	job := newJob("bound")
	expect(t, f.submit("alice-laptop", job, "proj-shared"), 200, "admitted at revision 1")
	f.revision = 2
	f.writePolicy()
	stale := map[string]string{"Zerus-Policy-Revision": "1"}
	expect(t, f.call("alice-laptop", "POST", "/executor/v1/jobs", f.request(newJob("stale"), "proj-shared"), stale), 409, "stale revision")
	expect(t, f.call("alice-laptop", "POST", "/executor/v1/jobs", f.request(newJob("nodeadline"), "proj-shared"), map[string]string{"Zerus-Request-Deadline": ""}), 409, "no deadline")
	far := strconv.FormatInt(f.now.UnixMilli()+10*60*1000, 10)
	expect(t, f.call("alice-laptop", "POST", "/executor/v1/jobs", f.request(newJob("far"), "proj-shared"), map[string]string{"Zerus-Request-Deadline": far}), 409, "deadline too far")
	past := strconv.FormatInt(f.now.UnixMilli()-1, 10)
	expect(t, f.call("alice-laptop", "POST", "/executor/v1/jobs", f.request(newJob("past"), "proj-shared"), map[string]string{"Zerus-Request-Deadline": past}), 409, "deadline passed")
	// The receipt admitted at revision 1 stays readable after the revision advanced.
	expect(t, f.call("alice-laptop", "GET", "/executor/v1/jobs/"+job, nil, nil), 200, "old receipt")
}

func TestRevocationAppliesToOpenRequestsAndQueuedJobs(t *testing.T) {
	f := newSharedFixture(t)
	job := newJob("queued")
	expect(t, f.submit("alice-laptop", job, "proj-shared"), 200, "alice submits")
	f.revision = 2
	f.revokedDevices = []string{f.devices["alice-laptop"].fingerprint}
	f.writePolicy()
	got := f.call("alice-laptop", "GET", "/executor/v1/jobs", nil, nil)
	if got.status != 403 || !strings.Contains(got.body, "device-revoked") {
		t.Fatalf("revoked device: %d %s", got.status, got.body)
	}
	// The queued job was admitted through the revoked device: it cannot launch.
	if !f.s.runNextShared(f.s.queue.Jobs) {
		t.Fatal("worker found nothing")
	}
	value, err := f.s.readSubmission(job)
	if err != nil || value.Phase != "access_refused" || !strings.Contains(value.Reason, "device-revoked") {
		t.Fatalf("queued job after revocation: %+v %v", value, err)
	}
	if _, err := os.Stat(filepath.Join(f.s.cfg.Root, "requests", job)); !os.IsNotExist(err) {
		t.Fatal("a refused job deployed")
	}
	// Alice's other device still reads it; the operator may release the
	// receipt, which never ran; bob still cannot see it.
	expect(t, f.call("alice-phone", "GET", "/executor/v1/jobs/"+job, nil, nil), 200, "alice's other device")
	release := map[string]any{"request_id": newJob("rel")[:32], "request_sha256": value.Digest}
	expect(t, f.call("bob-laptop", "POST", "/executor/v1/jobs/"+job+"/release", release, nil), 404, "bob releases")
	expect(t, f.call("carol-console", "POST", "/executor/v1/jobs/"+job+"/release", release, nil), 200, "operator releases")
	if len(f.s.queue.Jobs) != 0 {
		t.Fatalf("queue after release: %v", f.s.queue.Jobs)
	}
}

func TestARevokedPersonsQueuedJobDoesNotLaunch(t *testing.T) {
	f := newSharedFixture(t)
	job := newJob("person")
	expect(t, f.submit("bob-laptop", job, "proj-shared"), 200, "bob submits")
	f.revision = 2
	f.revokedPersons = []string{f.person("bob").Person}
	f.writePolicy()
	f.s.runNextShared(f.s.queue.Jobs)
	if value, _ := f.s.readSubmission(job); value.Phase != "access_refused" || !strings.Contains(value.Reason, "person-revoked") {
		t.Fatalf("%+v", value)
	}
}

func TestPolicyRollbackAndReplacementFailClosedAcrossRestart(t *testing.T) {
	f := newSharedFixture(t)
	f.revision = 3
	f.writePolicy()
	expect(t, f.call("alice-laptop", "GET", "/executor/v1/jobs", nil, nil), 200, "revision 3")
	f.revision = 2
	f.writePolicy()
	expect(t, f.call("alice-laptop", "GET", "/executor/v1/jobs", nil, nil), 503, "rollback")
	if _, err := f.s.currentPolicy(); err == nil || err.Error() != "policy-rollback" {
		t.Fatalf("rollback: %v", err)
	}
	// A restarted service (empty cache) still refuses the rollback.
	restarted := f.service(f.s.cfg)
	if _, err := restarted.currentPolicy(); err == nil || err.Error() != "policy-rollback" {
		t.Fatalf("rollback after restart: %v", err)
	}
	f.revision = 3
	f.revokedPersons = []string{f.person("bob").Person}
	f.writePolicy()
	if _, err := restarted.currentPolicy(); err == nil || err.Error() != "policy-replaced" {
		t.Fatalf("replaced revision: %v", err)
	}
	if err := os.Remove(f.policyPath); err != nil {
		t.Fatal(err)
	}
	expect(t, f.call("alice-laptop", "GET", "/executor/v1/jobs", nil, nil), 503, "missing policy")
}

func TestThePolicyFileIsARegularFileOutsideTheRoot(t *testing.T) {
	f := newSharedFixture(t)
	cfg := f.s.cfg
	inside := *cfg.Access
	inside.Policy = filepath.Join(cfg.Root, "access-policy.json")
	cfg.Access = &inside
	if err := validateAccessConfig(cfg); err == nil {
		t.Fatal("policy inside the executor root accepted")
	}
	relative := *f.s.cfg.Access
	relative.Policy = "access-policy.json"
	cfg.Access = &relative
	if err := validateAccessConfig(cfg); err == nil {
		t.Fatal("relative policy path accepted")
	}
	link := filepath.Join(filepath.Dir(f.policyPath), "linked-policy.json")
	if err := os.Symlink(f.policyPath, link); err == nil {
		if _, err := readPolicyFile(link, acceptTrust{}); err == nil {
			t.Fatal("followed a link to the policy")
		}
	} else {
		t.Logf("symbolic link not available here: %v", err)
	}
	if _, err := readPolicyFile(filepath.Dir(f.policyPath), acceptTrust{}); err == nil {
		t.Fatal("read a directory as the policy")
	}
}

func TestIsolationFailsClosedBeforeAnyWrite(t *testing.T) {
	for _, name := range []string{"", "none", "home", "env", "job-object", "JobObject", "windows-job-object", "windows-user-sid"} {
		if err := requireIsolation(name); err == nil {
			t.Errorf("%q accepted as isolation", name)
		}
	}
	// A production configuration naming a backend this build lacks refuses
	// before the executor writes anything to its root.
	base := t.TempDir()
	root := filepath.Join(base, "root")
	_ = os.Mkdir(root, 0700)
	operator := filepath.Join(base, "operator")
	_ = os.Mkdir(operator, 0700)
	cfg := map[string]any{"root": root, "peer": map[string]any{"identity": filepath.Join(operator, "identity")}, "serve_seconds": 60,
		"memory_bytes": 512 * MiB, "scenario_memory_bytes": 128 * MiB, "scenario_seconds": 60, "stash_bytes": 0,
		"access": map[string]any{"policy": filepath.Join(operator, "policy.json"), "operator": syntheticOperator, "isolation_backend": "windows-user-sid"}}
	path := filepath.Join(operator, "executor.json")
	serveWith := func(entry func(string) error, want string) {
		t.Helper()
		data, _ := json.Marshal(cfg)
		_ = os.WriteFile(path, data, 0600)
		err := entry(path)
		if err == nil || !strings.Contains(err.Error(), want) {
			t.Fatalf("serve: %v, want %s", err, want)
		}
		if entries, _ := os.ReadDir(root); len(entries) != 0 {
			t.Fatalf("refused executor wrote %d entries", len(entries))
		}
	}
	serve := func(want string) { t.Helper(); serveWith(serveSharedExecutor, want) }
	restoreBootstrap, restoreType := sharedBootstrap, sidAccountType
	t.Cleanup(func() { sharedBootstrap, sidAccountType = restoreBootstrap, restoreType })
	// A bootstrap refusal on the real configuration chain (a trust of nobody,
	// so the verdict is the same on every machine) stops the start before any
	// field of the configuration is used.
	sharedBootstrap = func(windows.Token) (policyTrust, error) {
		return &windowsPolicyTrust{nil, syntheticServiceToken(t)}, nil
	}
	serve("executor configuration: policy-owner-untrusted")
	// Past the bootstrap check (test seam), each later refusal still comes
	// before any write.
	sharedBootstrap = func(windows.Token) (policyTrust, error) { return acceptTrust{}, nil }
	sidAccountType = syntheticAccountTypes(restoreType)
	serve("not available in this build")
	// Without a named operator, with the executor's own account or a group as
	// the operator, the executor refuses; it never adopts the caller.
	access := cfg["access"].(map[string]any)
	access["isolation_backend"] = "synthetic-test"
	delete(access, "operator")
	serve("needs the named operator's SID")
	access["operator"] = currentUserSID(t).String()
	serve("operator-is-service")
	access["operator"] = syntheticGroup
	serve("operator-not-a-user")
	access["operator"] = syntheticUnknown
	serve("operator-unresolved")
	// The entries bind the mode: the single-person entry refuses an access
	// section, and the shared entry refuses a configuration without one.
	access["operator"] = syntheticOperator
	serveWith(serveExecutor, "starts only through executor-serve-shared")
	delete(cfg, "access")
	serve("needs an access section")
	cfg["access"] = access
	// A person whose context names another backend is refused at admission,
	// before a submission exists.
	f := newSharedFixture(t)
	f.revision = 2
	f.bobBackend = "windows-user-sid"
	f.writePolicy()
	got := f.submit("bob-laptop", newJob("ctx"), "proj-shared")
	if got.status != 403 || !strings.Contains(got.body, "context-backend") {
		t.Fatalf("bob without a qualified context: %d %s", got.status, got.body)
	}
	got = f.submit("carol-console", newJob("noctx"), "proj-shared")
	if got.status != 403 {
		t.Fatalf("carol: %d %s", got.status, got.body)
	}
}

func TestQueuesAreBoundedPerPersonAndServedFairly(t *testing.T) {
	f := newSharedFixture(t)
	expect(t, f.submit("alice-laptop", newJob("a1"), "proj-shared"), 200, "a1")
	expect(t, f.submit("alice-laptop", newJob("a2"), "proj-alice"), 200, "a2")
	expect(t, f.submit("alice-phone", newJob("a3"), "proj-shared"), 409, "alice's third job (per-person bound 2)")
	expect(t, f.submit("bob-laptop", newJob("b1"), "proj-shared"), 200, "bob is unaffected")

	owner := func(person string) *JobOwner { return &JobOwner{Person: person} }
	values := []ExecutorSubmission{
		{Request: ExecutorRequest{Job: "a1"}, Owner: owner("A")}, {Request: ExecutorRequest{Job: "a2"}, Owner: owner("A")},
		{Request: ExecutorRequest{Job: "a3"}, Owner: owner("A")}, {Request: ExecutorRequest{Job: "b1"}, Owner: owner("B")},
		{Request: ExecutorRequest{Job: "c1"}, Owner: owner("C")},
	}
	jobs := func(order []ExecutorSubmission) string {
		var ids []string
		for _, v := range order {
			ids = append(ids, v.Request.Job)
		}
		return strings.Join(ids, ",")
	}
	if got := jobs(fairOrder(values, "")); got != "a1,b1,c1,a2,a3" {
		t.Fatalf("fair order: %s", got)
	}
	if got := jobs(fairOrder(values, "A")); got != "b1,c1,a1,a2,a3" {
		t.Fatalf("fair order after A: %s", got)
	}
}

func TestCapabilitiesNameOnlyTheCaller(t *testing.T) {
	f := newSharedFixture(t)
	got := f.call("bob-laptop", "GET", "/executor/v1/capabilities", nil, nil)
	expect(t, got, 200, "capabilities")
	if !strings.Contains(got.body, f.person("bob").Person) || strings.Contains(got.body, f.person("alice").Person) ||
		strings.Contains(got.body, "proj-alice") || strings.Contains(got.body, "proj-shared") || !strings.Contains(got.body, `"cancel":"settled"`) {
		t.Fatalf("capabilities: %s", got.body)
	}
	expect(t, f.call("stranger", "GET", "/executor/v1/capabilities", nil, nil), 403, "unenrolled device")
}

func TestCancelHasRequestIdentityAndNoResultAccess(t *testing.T) {
	f := newSharedFixture(t)
	job := newJob("cancel")
	expect(t, f.submit("alice-laptop", job, "proj-shared"), 200, "alice submits")
	expect(t, f.call("alice-laptop", "POST", "/executor/v1/jobs/"+job+"/cancel", map[string]any{}, nil), 400, "cancel without request_id")
	expect(t, f.call("bob-laptop", "POST", "/executor/v1/jobs/"+job+"/cancel", map[string]any{"request_id": "00000000-0000-4000-8000-000000000001"}, nil), 404, "bob cancels")
	// A worker that read the job before the cancel.
	stale, _ := f.s.readSubmission(job)
	id := "00000000-0000-4000-8000-000000000002"
	got := f.call("alice-laptop", "POST", "/executor/v1/jobs/"+job+"/cancel", map[string]any{"request_id": id}, nil)
	if got.status != 200 || !strings.Contains(got.body, `"phase":"cancelled"`) || !strings.Contains(got.body, `"replayed":false`) {
		t.Fatalf("owner cancel: %d %s", got.status, got.body)
	}
	if err := f.s.phase(&stale, "deploying", ""); err == nil || stale.Phase != "cancelled" {
		t.Fatalf("a stale worker moved a cancelled job: %v %s", err, stale.Phase)
	}
	// Replay with the same identity, then another identity: one cancel only.
	got = f.call("alice-laptop", "POST", "/executor/v1/jobs/"+job+"/cancel", map[string]any{"request_id": id}, nil)
	if got.status != 200 || !strings.Contains(got.body, `"replayed":true`) {
		t.Fatalf("replay: %d %s", got.status, got.body)
	}
	got = f.call("alice-laptop", "POST", "/executor/v1/jobs/"+job+"/cancel", map[string]any{"request_id": "00000000-0000-4000-8000-000000000003"}, nil)
	if got.status != 200 || !strings.Contains(got.body, `"request_id":"`+id+`"`) || !strings.Contains(got.body, `"replayed":false`) {
		t.Fatalf("second identity: %d %s", got.status, got.body)
	}
	// The operator's resource-cancel reaches another person's job but never
	// its results.
	other := newJob("operator-cancel")
	expect(t, f.submit("alice-laptop", other, "proj-shared"), 200, "alice submits again")
	got = f.call("carol-console", "POST", "/executor/v1/jobs/"+other+"/cancel", map[string]any{"request_id": "00000000-0000-4000-8000-000000000004"}, nil)
	if got.status != 200 || !strings.Contains(got.body, `"phase":"cancelled"`) || strings.Contains(got.body, "log") || strings.Contains(got.body, "evidence") {
		t.Fatalf("operator cancel: %d %s", got.status, got.body)
	}
	expect(t, f.call("carol-console", "GET", "/executor/v1/jobs/"+other, nil, nil), 404, "operator reads")
	var record cancelRecord
	if _, err := readControl(filepath.Join(f.s.cfg.Root, "requests", other, "cancel.json"), &record); err != nil || record.Via != "operator" || record.Person != f.person("carol").Person {
		t.Fatalf("operator cancel record: %+v %v", record, err)
	}
	// A final receipt is not cancellable.
	done := newJob("done")
	expect(t, f.submit("bob-laptop", done, "proj-shared"), 200, "bob submits")
	value, _ := f.s.readSubmission(done)
	_ = f.s.phase(&value, "passed", "")
	got = f.call("bob-laptop", "POST", "/executor/v1/jobs/"+done+"/cancel", map[string]any{"request_id": "00000000-0000-4000-8000-000000000005"}, nil)
	if got.status != 409 || !strings.Contains(got.body, "not-cancellable") {
		t.Fatalf("final cancel: %d %s", got.status, got.body)
	}
	cancelRequests.Range(func(k, _ any) bool { cancelRequests.Delete(k); return true })
}

// A cancel requested while a job deploys stops it before launch.
func TestCancelDuringDeploymentPreventsTheLaunch(t *testing.T) {
	f := newSharedFixture(t)
	job := newJob("deploy-cancel")
	expect(t, f.submit("alice-laptop", job, "proj-shared"), 200, "submit")
	launched := false
	f.s.deliverFn = func(value ExecutorSubmission, offer TreeOffer, kind string) (string, string, error) {
		if kind == "slots" {
			got := f.call("alice-laptop", "POST", "/executor/v1/jobs/"+job+"/cancel", map[string]any{"request_id": "00000000-0000-4000-8000-000000000006"}, nil)
			if got.status != 200 {
				t.Errorf("cancel during deploy: %d %s", got.status, got.body)
			}
		}
		return filepath.Join(f.s.cfg.Root, "fake-"+kind), strings.Repeat("e", 32), nil
	}
	f.s.launchFn = func(string, string) error { launched = true; return nil }
	value, _ := f.s.readSubmission(job)
	f.s.runOne(value)
	after, _ := f.s.readSubmission(job)
	if launched || after.Phase != "cancelled" {
		t.Fatalf("launched=%v phase=%s", launched, after.Phase)
	}
	cancelRequests.Delete(job)
}

func TestOwnerlessReceiptsAreNotAdoptedAndTheModeIsSticky(t *testing.T) {
	f := newSharedFixture(t)
	if err := refuseLegacyAfterShared(f.s.cfg.Root); err == nil {
		t.Fatal("a multi-person root may return to the single-pin mode")
	}
	// Plant an ownerless receipt in the queue: a restart refuses to adopt it.
	legacyCfg := f.s.cfg
	legacyCfg.Access = nil
	legacy := f.service(legacyCfg)
	request := f.request(newJob("legacy"), "")
	value := ExecutorSubmission{deliveryProtocol, f.s.identity, f.s.root, executorRequestDigest(request), request, "queued", "", time.Now().UnixMilli(), nil}
	if err := legacy.saveSubmission(value); err != nil {
		t.Fatal(err)
	}
	f.s.queue.Jobs = append(f.s.queue.Jobs, request.Job)
	if err := f.s.prepareAccessRoot(); err == nil {
		t.Fatal("adopted an ownerless receipt")
	}
	// Nor does a caller reach it through its ID.
	expect(t, f.call("alice-laptop", "GET", "/executor/v1/jobs/"+request.Job, nil, nil), 404, "ownerless receipt")
	expect(t, f.submit("alice-laptop", request.Job, "proj-shared"), 409, "claim the ownerless ID")
}

func TestTheListenerAdmitsOnlyCurrentlyEnrolledDevices(t *testing.T) {
	f := newSharedFixture(t)
	f.s.access.now = nil // the TLS check uses the real clock
	listener, endpoint, pin, err := f.s.accessListener()
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	server := &http.Server{Handler: f.s}
	go server.Serve(listener)
	defer server.Close()
	client := func(device string) *http.Client {
		return &http.Client{Timeout: 10 * time.Second, Transport: &http.Transport{TLSClientConfig: &tls.Config{MinVersion: tls.VersionTLS13,
			Certificates: []tls.Certificate{f.devices[device].pair}, InsecureSkipVerify: true,
			VerifyConnection: func(state tls.ConnectionState) error { return verifyPeer(pin, x509.ExtKeyUsageServerAuth)(state) }}}}
	}
	alice := client("alice-laptop")
	answer, err := alice.Get(endpoint + "/executor/v1/capabilities")
	if err != nil || answer.StatusCode != 200 {
		t.Fatalf("alice: %v %v", answer, err)
	}
	answer.Body.Close()
	if _, err := client("stranger").Get(endpoint + "/executor/v1/capabilities"); err == nil {
		t.Fatal("an unenrolled device connected")
	}
	// Revoke alice's device: her open connection's next request is refused.
	f.revision = 2
	f.revokedDevices = []string{f.devices["alice-laptop"].fingerprint}
	f.writePolicy()
	answer, err = alice.Get(endpoint + "/executor/v1/jobs")
	if err == nil {
		body, _ := io.ReadAll(answer.Body)
		answer.Body.Close()
		if answer.StatusCode != 403 || !strings.Contains(string(body), "device-revoked") {
			t.Fatalf("revoked open connection: %d %s", answer.StatusCode, body)
		}
	}
	alice.CloseIdleConnections()
	if _, err := alice.Get(endpoint + "/executor/v1/jobs"); err == nil {
		t.Fatal("a revoked device reconnected")
	}
}

func TestSlotsBelongToTheJobsProject(t *testing.T) {
	legacy := ExecutorSubmission{Request: ExecutorRequest{Slot: "slot-a"}}
	shared := ExecutorSubmission{Request: ExecutorRequest{Slot: "slot-a"}, Owner: &JobOwner{Person: "p", Project: "proj-shared"}}
	other := ExecutorSubmission{Request: ExecutorRequest{Slot: "slot-a"}, Owner: &JobOwner{Person: "q", Project: "proj-alice"}}
	partner := ExecutorSubmission{Request: ExecutorRequest{Slot: "slot-a"}, Owner: &JobOwner{Person: "q", Project: "proj-shared"}}
	if slotRelative(legacy, "slots") != "slots/slot-a" {
		t.Fatal("the single-person layout changed")
	}
	if slotRelative(shared, "slots") == slotRelative(other, "slots") || slotRelative(shared, "bundles") == slotRelative(other, "bundles") {
		t.Fatal("two projects share a slot folder")
	}
	if slotRelative(shared, "slots") != slotRelative(partner, "slots") {
		t.Fatal("members of one project do not share its managed copy")
	}
}
