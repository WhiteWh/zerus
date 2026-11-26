package main

import (
	"crypto/ed25519"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func personSeed(t *testing.T, f *sharedFixture, name string) []byte {
	seed, err := hex.DecodeString(f.persons[name].Seed)
	if err != nil {
		t.Fatal(err)
	}
	return seed
}

func writeJSON(t *testing.T, path string, value any) string {
	t.Helper()
	data, _ := json.Marshal(value)
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestEnrollmentRequestsAreHostBoundSignedAndBounded(t *testing.T) {
	f := newSharedFixture(t)
	wire, err := readPolicyForAdmin(f.policyPath)
	if err != nil {
		t.Fatal(err)
	}
	now := uint64(time.Now().Unix())
	device := newTestDevice(t)
	good, err := signEnrollment(personSeed(t, f, "alice"), wire.Host, device.fingerprint, "alice", now, now+30*24*3600)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := checkEnrollmentRequest(good, wire, now); err != nil {
		t.Fatalf("valid request: %v", err)
	}
	bobSigned, _ := signEnrollment(personSeed(t, f, "bob"), wire.Host, device.fingerprint, "alice", now, now+3600)
	for name, c := range map[string]struct {
		change func(*enrollmentRequest)
		at     uint64
		code   string
	}{
		"other host":           {func(r *enrollmentRequest) { r.Host = strings.Repeat("0", 64) }, now, "request-host-mismatch"},
		"body person claim":    {func(r *enrollmentRequest) { r.Person = f.person("bob").Person }, now, "request-identity"},
		"another key's sig":    {func(r *enrollmentRequest) { r.Signature = bobSigned.Signature }, now, "request-signature"},
		"edited expiry":        {func(r *enrollmentRequest) { r.Expires += 3600 }, now, "request-signature"},
		"too long":             {func(r *enrollmentRequest) { r.Expires = r.Issued + maxEnrollmentLifetime + 1 }, now, "request-window"},
		"stale":                {func(*enrollmentRequest) {}, now + maxEnrollmentAge + 1, "request-stale"},
		"issued in the future": {func(*enrollmentRequest) {}, now - 3600, "request-stale"},
		"wrong kind":           {func(r *enrollmentRequest) { r.Kind = "policy" }, now, "request-contract"},
		"bad label":            {func(r *enrollmentRequest) { r.Label = "" }, now, "request-identity"},
	} {
		r := good
		c.change(&r)
		if _, err := checkEnrollmentRequest(r, wire, c.at); err == nil || err.Error() != c.code {
			t.Errorf("%s: %v, want %s", name, err, c.code)
		}
	}
	revoked := wire
	revoked.RevokedDevices = append([]string{}, device.fingerprint)
	if _, err := checkEnrollmentRequest(good, revoked, now); err == nil || err.Error() != "request-device-revoked" {
		t.Fatalf("revoked device: %v", err)
	}
	revoked = wire
	revoked.RevokedPersons = []string{good.Person}
	if _, err := checkEnrollmentRequest(good, revoked, now); err == nil || err.Error() != "request-person-revoked" {
		t.Fatalf("revoked person: %v", err)
	}
	existing, _ := signEnrollment(personSeed(t, f, "alice"), wire.Host, f.devices["alice-laptop"].fingerprint, "alice", now, now+3600)
	if _, err := checkEnrollmentRequest(existing, wire, now); err == nil || err.Error() != "request-duplicate-device" {
		t.Fatalf("duplicate device: %v", err)
	}
}

// End to end through the commands: a device request, single-use admission
// to a new policy revision that the executor accepts, replay refusal,
// rotation and revocation.
func TestAdmissionWritesTheNextRevisionOnceAndTheExecutorAcceptsIt(t *testing.T) {
	f := newSharedFixture(t)
	dir := t.TempDir()
	seedFile := filepath.Join(dir, "person.seed")
	if err := os.WriteFile(seedFile, []byte(base64.RawURLEncoding.EncodeToString(personSeed(t, f, "alice"))+"\n"), 0600); err != nil {
		t.Fatal(err)
	}
	device := newTestDevice(t)
	certFile := filepath.Join(dir, "device.pem")
	if err := os.WriteFile(certFile, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: device.cert.Raw}), 0600); err != nil {
		t.Fatal(err)
	}
	request := filepath.Join(dir, "request.json")
	if err := runRequestDevice(writeJSON(t, filepath.Join(dir, "req.cfg"), requestDeviceConfig{seedFile, f.host, certFile, "alice", 30, request})); err != nil {
		t.Fatal(err)
	}
	data, _ := os.ReadFile(request)
	if strings.Contains(string(data), f.persons["alice"].Seed) || strings.Contains(string(data), base64.RawURLEncoding.EncodeToString(personSeed(t, f, "alice"))) {
		t.Fatal("the request carries the person's private key")
	}
	if err := runRequestDevice(writeJSON(t, filepath.Join(dir, "req2.cfg"), requestDeviceConfig{seedFile, f.host, certFile, "alice", 30, request})); err == nil {
		t.Fatal("overwrote an existing request file")
	}
	ledger := filepath.Join(dir, "ledger.json")
	next := filepath.Join(dir, "policy-2.json")
	admit := admitDeviceConfig{PolicyIn: f.policyPath, PolicyOut: next, Request: request, Ledger: ledger}
	if err := runAdmitDevice(writeJSON(t, filepath.Join(dir, "admit.cfg"), admit)); err != nil {
		t.Fatal(err)
	}
	wire, err := readPolicyForAdmin(next)
	if err != nil || wire.Revision != f.revision+1 {
		t.Fatalf("next policy: %+v %v", wire.Revision, err)
	}
	// Replaying the same request against the old revision is refused by the
	// ledger, even though the device is absent there.
	admit.PolicyOut = filepath.Join(dir, "policy-2b.json")
	if err := runAdmitDevice(writeJSON(t, filepath.Join(dir, "admit2.cfg"), admit)); err == nil || err.Error() != "request-replayed" {
		t.Fatalf("replay: %v", err)
	}
	// The executor accepts the new device once the operator installs the
	// revision, and the device sees only alice's scope.
	installed, _ := os.ReadFile(next)
	if err := os.WriteFile(f.policyPath, installed, 0600); err != nil {
		t.Fatal(err)
	}
	f.revision = wire.Revision
	f.now = time.Now().Add(time.Second)
	f.devices["alice-new"] = device
	got := f.call("alice-new", "GET", "/executor/v1/capabilities", nil, nil)
	if got.status != 200 || !strings.Contains(got.body, f.person("alice").Person) {
		t.Fatalf("new device: %d %s", got.status, got.body)
	}
	// Rotation: a newer device replaces the old one in one revision.
	newer := newTestDevice(t)
	now := uint64(time.Now().Unix())
	rotation, _ := signEnrollment(personSeed(t, f, "alice"), f.host, newer.fingerprint, "alice", now, now+3600)
	rotationFile := writeJSON(t, filepath.Join(dir, "rotation.json"), rotation)
	third := filepath.Join(dir, "policy-3.json")
	if err := runAdmitDevice(writeJSON(t, filepath.Join(dir, "rotate.cfg"), admitDeviceConfig{PolicyIn: next, PolicyOut: third, Request: rotationFile, Ledger: ledger, Revoke: []string{device.fingerprint}})); err != nil {
		t.Fatal(err)
	}
	// Rotating away another person's device is refused.
	bobs, _ := signEnrollment(personSeed(t, f, "alice"), f.host, newTestDevice(t).fingerprint, "alice", now, now+3600)
	if _, err := admitRequest(wire, bobs, nil, nil, []string{f.devices["bob-laptop"].fingerprint}); err == nil || err.Error() != "rotation-not-owned" {
		t.Fatalf("rotating another person's device: %v", err)
	}
	installed, _ = os.ReadFile(third)
	_ = os.WriteFile(f.policyPath, installed, 0600)
	f.revision++
	f.now = time.Now().Add(time.Second)
	f.devices["alice-newer"] = newer
	expect(t, f.call("alice-new", "GET", "/executor/v1/capabilities", nil, nil), 403, "rotated-away device")
	expect(t, f.call("alice-newer", "GET", "/executor/v1/capabilities", nil, nil), 200, "rotated-in device")
	// Revocation of the person through the command.
	fourth := filepath.Join(dir, "policy-4.json")
	if err := runRevoke(writeJSON(t, filepath.Join(dir, "revoke.cfg"), revokeConfig{PolicyIn: third, PolicyOut: fourth, Persons: []string{f.person("alice").Person}})); err != nil {
		t.Fatal(err)
	}
	installed, _ = os.ReadFile(fourth)
	_ = os.WriteFile(f.policyPath, installed, 0600)
	f.revision++
	expect(t, f.call("alice-newer", "GET", "/executor/v1/capabilities", nil, nil), 403, "revoked person")
	expect(t, f.call("alice-laptop", "GET", "/executor/v1/capabilities", nil, nil), 403, "revoked person's other device")
	expect(t, f.call("bob-laptop", "GET", "/executor/v1/capabilities", nil, nil), 200, "another person")
}

// Two people with the same label stay distinct; the summary shows the
// verified host and scope without keys or signatures.
func TestIdenticalLabelsStayDistinctPeople(t *testing.T) {
	f := newSharedFixture(t)
	wire, err := readPolicyForAdmin(f.policyPath)
	if err != nil {
		t.Fatal(err)
	}
	now := uint64(time.Now().Unix())
	seed := make([]byte, ed25519.SeedSize)
	seed[0] = 42
	twin, _ := signEnrollment(seed, wire.Host, newTestDevice(t).fingerprint, wire.Persons[0].Label, now, now+3600)
	if _, err := checkEnrollmentRequest(twin, wire, now); err != nil {
		t.Fatal(err)
	}
	next, err := admitRequest(wire, twin, []admissionProject{{"proj-shared", "reader"}}, nil, nil)
	if err != nil {
		t.Fatal(err)
	}
	summary := summarizePolicy(next)
	labels := map[string][]string{}
	for _, p := range summary.Persons {
		labels[p.Label] = append(labels[p.Label], p.Person)
	}
	if len(labels[wire.Persons[0].Label]) != 2 || labels[wire.Persons[0].Label][0] == labels[wire.Persons[0].Label][1] {
		t.Fatalf("labels: %v", labels)
	}
	out, _ := json.Marshal(summary)
	if strings.Contains(string(out), twin.Signature) || strings.Contains(string(out), twin.PublicKey) || summary.Host != wire.Host {
		t.Fatalf("summary leaks or lacks the host: %s", out)
	}
}

// The shared fixture: Go and Rust check enrollment requests alike.
func TestEveryEnrollmentRequestFixtureMatches(t *testing.T) {
	var policies struct {
		Valid []struct{ Name, Text string }
	}
	hostAccessFixture(t, "policies.json", &policies)
	wires := map[string]wirePolicy{}
	for _, p := range policies.Valid {
		var wire wirePolicy
		if err := json.Unmarshal([]byte(p.Text), &wire); err != nil {
			t.Fatal(err)
		}
		wires[p.Name] = wire
	}
	var fixture struct {
		Now   uint64
		Cases []struct {
			Name, Policy, Code string
			Request            enrollmentRequest
		}
	}
	hostAccessFixture(t, "enrollment-requests.json", &fixture)
	if len(fixture.Cases) < 15 {
		t.Fatalf("only %d request cases", len(fixture.Cases))
	}
	for _, c := range fixture.Cases {
		_, err := checkEnrollmentRequest(c.Request, wires[c.Policy], fixture.Now)
		got := ""
		if err != nil {
			got = err.Error()
		}
		if got != c.Code {
			t.Errorf("%s: %q, want %q", c.Name, got, c.Code)
		}
	}
}
