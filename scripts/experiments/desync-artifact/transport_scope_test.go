package main

import "testing"

func TestTransportScopeFollowsValidatedEndpoint(t *testing.T) {
	pin := "aa00000000000000000000000000000000000000000000000000000000000000"
	cases := []struct {
		endpoint, pin, transport string
		external                 any
	}{
		{"http://127.0.0.1:4100", "", "authenticated-loopback-only", false},
		{"https://127.0.0.1:4100", pin, "paired-tls-loopback", false},
		{"https://127.8.0.3:4100/tree/v1/files/abc", pin, "paired-tls-loopback", false},
		{"https://10.20.0.12:7443", pin, "paired-tls-private-network", true},
		{"https://192.168.1.20:7443/tree/v1/files/abc", pin, "paired-tls-private-network", true},
		{"https://172.16.4.5:7443", pin, "paired-tls-private-network", true},
		// Never labelled loopback: a pinned peer over plain HTTP, an unpinned
		// HTTPS peer, a public or non-IPv4 host, or an unparsable endpoint.
		{"http://10.20.0.12:7443", pin, "unverified", nil},
		{"http://10.20.0.12:7443", "", "unverified", nil},
		{"https://10.20.0.12:7443", "", "unverified", nil},
		{"http://127.0.0.1:4100", pin, "unverified", nil},
		{"https://8.8.8.8:443", pin, "unverified", nil},
		{"https://[::1]:443", pin, "unverified", nil},
		{"https://example.com:443", pin, "unverified", nil},
		{"::", pin, "unverified", nil},
	}
	for _, c := range cases {
		scope := transportScope(c.endpoint, c.pin)
		if scope["transport"] != c.transport || scope["external_network"] != c.external {
			t.Errorf("%s (pinned %v): got %v", c.endpoint, c.pin != "", scope)
		}
	}
}

func TestTransportScopeOverridesTheLocalDefault(t *testing.T) {
	values := withScope(transportScope("https://10.0.0.2:7443", "ab"), map[string]any{"downloaded_payload_bytes": int64(7)})
	result := map[string]any{"transport": "authenticated-loopback-only", "external_network": false}
	for key, value := range values {
		result[key] = value
	}
	if result["transport"] != "paired-tls-private-network" || result["external_network"] != true || result["downloaded_payload_bytes"] != int64(7) {
		t.Fatalf("remote scope did not replace the local default: %v", result)
	}
}
