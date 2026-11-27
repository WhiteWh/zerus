package main

import (
	"crypto/tls"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// sharedServer serves the fixture's executor over TLS; client is device's.
func sharedServer(t *testing.T, f *sharedFixture, device string) executorClient {
	t.Helper()
	server := httptest.NewUnstartedServer(f.s)
	server.TLS = &tls.Config{ClientAuth: tls.RequireAnyClientCert}
	server.StartTLS()
	t.Cleanup(server.Close)
	client := server.Client()
	transport := client.Transport.(*http.Transport).Clone()
	transport.TLSClientConfig.Certificates = []tls.Certificate{f.devices[device].pair}
	client.Transport = transport
	return executorClient{client: client, endpoint: server.URL, shared: true}
}

func TestAControllerSubmitsToAMultiPersonExecutor(t *testing.T) {
	f := newSharedFixture(t)
	alice := sharedServer(t, f, "alice-laptop")
	job := newJob("controller")
	if _, _, status, err := alice.request("POST", "/executor/v1/jobs", f.request(job, "proj-shared"), nil, maxControlBytes); err != nil {
		t.Fatalf("submit: %d %v", status, err)
	}
	if _, status, err := alice.snapshot(job); err != nil {
		t.Fatalf("alice reads her job: %d %v", status, err)
	}
	bob := sharedServer(t, f, "bob-laptop")
	if _, status, err := bob.snapshot(job); err == nil || status != 404 {
		t.Fatalf("bob sees alice's job: %d %v", status, err)
	}
	unbound := alice
	unbound.shared = false
	if _, _, status, _ := unbound.request("POST", "/executor/v1/jobs", f.request(newJob("unbound"), "proj-shared"), nil, maxControlBytes); status != http.StatusConflict {
		t.Fatalf("a mutation without the revision binding was not refused: %d", status)
	}
}

func TestAControllerWithoutAProjectIsRefusedByAMultiPersonExecutor(t *testing.T) {
	f := newSharedFixture(t)
	alice := sharedServer(t, f, "alice-laptop")
	_, _, status, err := alice.request("POST", "/executor/v1/jobs", f.request(newJob("no project"), ""), nil, maxControlBytes)
	if err == nil || status == 200 || !strings.Contains(err.Error(), "refused") {
		t.Fatalf("submit without a project: %d %v", status, err)
	}
}
