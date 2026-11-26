package main

import (
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
)

// Host compatibility below the qualified kernel.
//
// The distributable record proves this exact binary on the windows-2025
// runner (build 26100). A Windows 11 host from build 22000 up to that kernel
// is accepted only after this host itself passes the kernel-dependent cases
// of the same qualification, run by the executor service in a fresh folder
// under its own root at every activation: Q0 verified contained launch and
// profile collision, Q1 file isolation, Q2 folder rights, Q3 named objects,
// processes, registry and loopback, Q4 and Q5 network denial and Q6 the Job
// lifetime. The cases that do not depend on the kernel (Q7 and Q9 cancel and
// leases, Q8 policy trust, Q10 tools) stay covered by the record. No account
// is created; container profiles are created and removed by the service, as
// in ordinary operation.
const minimumProbedBuild = 22000

// hostNeedsProbe reports whether host is older than the qualified kernel.
func hostNeedsProbe(host string) bool {
	_, _, build, ok := parseOSVersion(host)
	return ok && build < minimumQualifiedBuild
}

// hostCompatibilityProbe is runHostProbe; tests substitute it.
var hostCompatibilityProbe = runHostProbe

func runHostProbe(stateRoot string) error {
	if stateRoot == "" {
		return errors.New("no executor root for the host probe")
	}
	nonce, err := newIdentity()
	if err != nil {
		return err
	}
	root := filepath.Join(stateRoot, "host-qualification-"+nonce)
	if err := os.Mkdir(root, 0700); err != nil {
		return err
	}
	defer os.RemoveAll(root)
	ip := privateIPv4()
	q := &qualifier{cfg: qualifyConfig{Root: root, ListenIP: ip}, installRoot: stateRoot}
	if err := q.setup(); err != nil {
		return fmt.Errorf("host probe setup: %w", err)
	}
	defer func() {
		for _, name := range []string{q.plan.Name, q.other.Name} {
			if name != "" {
				_ = (windowsProfiles{}).remove(name)
			}
		}
	}()
	q.record("Q0", q.q0())
	files, objects, network := q.q1q3q5()
	if ip == "" {
		// No private network on this host: there is no LAN to deny.
		network = nil
	}
	q.record("Q1", files)
	q.record("Q2", q.q2())
	q.record("Q3", objects)
	q.record("Q4", q.q4())
	q.record("Q5", network)
	q.record("Q6", q.q6())
	result := map[string]any{"protocol": qualificationProtocol, "os_version": osVersion(), "cases": q.cases}
	_ = durableJSON(filepath.Join(stateRoot, "host-qualification.json"), result)
	for _, c := range q.cases {
		if c.Verdict != "pass" {
			return fmt.Errorf("%s %s: %s", c.Case, c.Verdict, c.Detail)
		}
	}
	return nil
}

// privateIPv4 is a private, non-loopback IPv4 address of this host, if any.
func privateIPv4() string {
	addresses, err := net.InterfaceAddrs()
	if err != nil {
		return ""
	}
	for _, address := range addresses {
		if ipnet, ok := address.(*net.IPNet); ok {
			if ip := ipnet.IP.To4(); ip != nil && ip.IsPrivate() && !ip.IsLoopback() {
				return ip.String()
			}
		}
	}
	return ""
}
