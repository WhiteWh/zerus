package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// The shared contracts/host-access-v1 fixtures, copied to testdata by build.py
// (src/host_access reads the same files).
func hostAccessFixture(t *testing.T, name string, value any) {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("testdata", "host-access-v1", name))
	if err != nil {
		t.Fatalf("shared fixture missing (build.py --test copies it): %v", err)
	}
	if err := json.Unmarshal(data, value); err != nil {
		t.Fatal(err)
	}
}

type policyFixture struct {
	Name     string `json:"name"`
	Text     string `json:"text"`
	PadTo    int    `json:"pad_to_bytes"`
	Expected string `json:"expect"`
}

func (f policyFixture) bytes() []byte {
	data := []byte(f.Text)
	for len(data) < f.PadTo {
		data = append(data, ' ')
	}
	return data
}

type policyFixtures struct {
	Host    string          `json:"host"`
	Valid   []policyFixture `json:"valid"`
	Invalid []policyFixture `json:"invalid"`
}

func TestEveryInvalidPolicyFixtureFailsWithItsCode(t *testing.T) {
	var fixtures policyFixtures
	hostAccessFixture(t, "policies.json", &fixtures)
	if len(fixtures.Invalid) < 50 {
		t.Fatalf("only %d invalid fixtures", len(fixtures.Invalid))
	}
	for _, f := range fixtures.Invalid {
		_, err := verifyAccessPolicy(f.bytes(), fixtures.Host)
		if err == nil || err.Error() != f.Expected {
			t.Errorf("%s: got %v, want %s", f.Name, err, f.Expected)
		}
	}
}

func TestValidPolicyFixturesVerifyAndBindTheirHost(t *testing.T) {
	var fixtures policyFixtures
	hostAccessFixture(t, "policies.json", &fixtures)
	for _, f := range fixtures.Valid {
		policy, err := verifyAccessPolicy(f.bytes(), fixtures.Host)
		if err != nil || policy.Host != fixtures.Host {
			t.Fatalf("%s: %v", f.Name, err)
		}
		if _, err := verifyAccessPolicy(f.bytes(), strings.Repeat("0", 64)); err == nil || err.Error() != "host-mismatch" {
			t.Fatalf("%s verified for another host: %v", f.Name, err)
		}
	}
}

func TestEveryDecisionFixtureMatches(t *testing.T) {
	var fixtures policyFixtures
	hostAccessFixture(t, "policies.json", &fixtures)
	policies := map[string]*AccessPolicy{}
	for _, f := range fixtures.Valid {
		policy, err := verifyAccessPolicy(f.bytes(), fixtures.Host)
		if err != nil {
			t.Fatal(err)
		}
		policies[f.Name] = policy
	}
	var decisions struct {
		Cases []struct {
			Name, Policy, Device, Operation, Project string
			Now                                      uint64
			Expect                                   struct {
				Allow                   bool
				Person, Role, Via, Code string
			}
		}
	}
	hostAccessFixture(t, "decisions.json", &decisions)
	if len(decisions.Cases) < 20 {
		t.Fatal("decision fixtures missing")
	}
	for _, c := range decisions.Cases {
		decision, err := authorizeAccess(policies[c.Policy], c.Device, c.Now, c.Operation, c.Project)
		if c.Expect.Allow {
			if err != nil || decision.Person != c.Expect.Person || decision.Role.name() != c.Expect.Role || decision.Via != c.Expect.Via {
				t.Errorf("%s: got %v %v, want %+v", c.Name, decision, err, c.Expect)
			}
		} else if err == nil || err.Error() != c.Expect.Code {
			t.Errorf("%s: got %v %v, want %s", c.Name, decision, err, c.Expect.Code)
		}
	}
}

func TestTheMobileProtocolsRejectedKeysAreRejectedAsPersonKeys(t *testing.T) {
	data, err := os.ReadFile(filepath.Join("testdata", "mobile-vectors", "rejected-keys-v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	var vectors struct {
		Keys []string `json:"ed25519_rejected_public_keys"`
	}
	if err := json.Unmarshal(data, &vectors); err != nil || len(vectors.Keys) < 40 {
		t.Fatal("vectors missing")
	}
	for _, key := range vectors.Keys {
		if _, ok := personKey(key); ok {
			t.Errorf("accepted %s", key)
		}
	}
}

func TestTheStatementIsTheDocumentedLineFormat(t *testing.T) {
	text := string(enrollmentStatement(strings.Repeat("a", 64), strings.Repeat("b", 64), strings.Repeat("c", 64), 1, 20))
	if !strings.HasPrefix(text, "zerus-host-access-v1/device-enrollment\nhost=") || !strings.HasSuffix(text, "\nissued=1\nexpires=20\n") || strings.Contains(text, "\r") {
		t.Fatalf("%q", text)
	}
}

func TestEveryPersonDecisionFixtureMatches(t *testing.T) {
	var policies struct {
		Host  string
		Valid []struct{ Name, Text string }
	}
	hostAccessFixture(t, "policies.json", &policies)
	verified := map[string]*AccessPolicy{}
	for _, p := range policies.Valid {
		policy, err := verifyAccessPolicy([]byte(p.Text), policies.Host)
		if err != nil {
			t.Fatal(err)
		}
		verified[p.Name] = policy
	}
	var fixture struct {
		Cases []struct {
			Name, Policy, Person, Operation, Project string
			Expect                                   struct {
				Allow                    bool
				Person, Role, Via, Code string
			}
		}
	}
	hostAccessFixture(t, "person-decisions.json", &fixture)
	if len(fixture.Cases) < 16 {
		t.Fatalf("only %d cases", len(fixture.Cases))
	}
	for _, c := range fixture.Cases {
		decision, err := authorizePerson(verified[c.Policy], c.Person, c.Operation, c.Project)
		if c.Expect.Allow {
			if err != nil || decision.Person != c.Expect.Person || decision.Role.name() != c.Expect.Role || decision.Via != c.Expect.Via {
				t.Errorf("%s: %v %v", c.Name, decision, err)
			}
		} else if err == nil || err.Error() != c.Expect.Code {
			t.Errorf("%s: %v, want %s", c.Name, err, c.Expect.Code)
		}
	}
}
