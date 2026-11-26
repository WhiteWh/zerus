package main

import "testing"

// A Windows 11 host below the qualified kernel accepts the runner's record
// only with its own host probe; Windows 10 and other versions never do.
func TestOlderWindows11HostsNeedTheirOwnProbe(t *testing.T) {
	binary := "ab" + "cd"
	for len(binary) < 64 {
		binary += "ab"
	}
	r := passingRecord(binary)
	for host, want := range map[string]string{
		"10.0.22000": "", "10.0.22631": "", "10.0.26100": "", "10.0.26300": "",
		"10.0.21999": "qualification-host-incompatible", "10.0.19045": "qualification-host-incompatible",
		"11.0.30000": "qualification-host-incompatible",
	} {
		err := checkQualificationRecordOn(r, binary, fixtureSource, host)
		if (want == "" && err != nil) || (want != "" && (err == nil || err.Error() != want)) {
			t.Errorf("%s: %v, want %q", host, err, want)
		}
	}
	for host, want := range map[string]bool{"10.0.22000": true, "10.0.22631": true, "10.0.26099": true, "10.0.26100": false, "10.0.26300": false} {
		if hostNeedsProbe(host) != want {
			t.Errorf("hostNeedsProbe(%s) = %v", host, !want)
		}
	}
}
