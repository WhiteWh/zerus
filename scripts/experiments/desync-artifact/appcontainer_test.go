package main

// Local tests of the AppContainer candidate's pure parts. They derive names
// and SIDs (pure hashes) and inspect this process's own token; they create no
// profile, change no ACL and launch nothing contained.

import (
	"strings"
	"testing"

	"golang.org/x/sys/windows"
)

var (
	acInstallation = strings.Repeat("a1", 32)
	acHost         = strings.Repeat("b2", 32)
	acPerson       = strings.Repeat("c3", 32)
	acOtherPerson  = strings.Repeat("d4", 32)
)

func TestContainerNamesAreDeterministicAndScoped(t *testing.T) {
	base, err := containerName(acInstallation, acHost, acPerson, "builds")
	if err != nil {
		t.Fatal(err)
	}
	again, _ := containerName(acInstallation, acHost, acPerson, "builds")
	if base != again || len(base) > 64 || !strings.HasPrefix(base, "Zerus.AC.") {
		t.Fatalf("name %q / %q", base, again)
	}
	for _, other := range [][4]string{
		{strings.Repeat("e5", 32), acHost, acPerson, "builds"},
		{acInstallation, strings.Repeat("e5", 32), acPerson, "builds"},
		{acInstallation, acHost, acOtherPerson, "builds"},
		{acInstallation, acHost, acPerson, "tests"},
	} {
		name, err := containerName(other[0], other[1], other[2], other[3])
		if err != nil || name == base {
			t.Fatalf("scope %v did not change the name: %q %v", other, name, err)
		}
	}
	for _, bad := range [][4]string{
		{"", acHost, acPerson, "builds"},
		{strings.ToUpper(acInstallation), acHost, acPerson, "builds"},
		{acInstallation, acHost, "S-1-15-2-1", "builds"},
		{acInstallation, acHost, acPerson, "../x"},
	} {
		if _, err := containerName(bad[0], bad[1], bad[2], bad[3]); err == nil {
			t.Fatalf("accepted %v", bad)
		}
	}
}

func TestContainerAndCapabilitySIDsAreDerived(t *testing.T) {
	name, _ := containerName(acInstallation, acHost, acPerson, "builds")
	sid, err := appContainerSID(name)
	if err != nil {
		t.Fatal(err)
	}
	again, _ := appContainerSID(name)
	other, _ := appContainerSID(name + "x")
	if !strings.HasPrefix(sid.String(), "S-1-15-2-") || !sid.Equals(again) || sid.Equals(other) {
		t.Fatalf("package SIDs %s %s %s", sid, again, other)
	}
	capName, err := projectCapabilityName(acInstallation, "proj-shared")
	if err != nil {
		t.Fatal(err)
	}
	capSID, err := capabilitySID(capName)
	if err != nil {
		t.Fatal(err)
	}
	otherCap, _ := projectCapabilityName(acInstallation, "proj-other")
	otherSID, _ := capabilitySID(otherCap)
	if !strings.HasPrefix(capSID.String(), "S-1-15-3-1024-") || capSID.Equals(otherSID) {
		t.Fatalf("capability SIDs %s %s", capSID, otherSID)
	}
}

func TestThePlanComesOnlyFromTheVerifiedContext(t *testing.T) {
	policy := &AccessPolicy{Contexts: map[string]accessContext{
		acPerson:      {"builds", appContainerBackend},
		acOtherPerson: {"builds", "windows-local-user-v1"},
	}}
	owner := JobOwner{Person: acPerson, Project: "proj-shared"}
	plan, err := planContainer(acInstallation, acHost, owner, policy, networkGrant{})
	if err != nil {
		t.Fatal(err)
	}
	wantName, _ := containerName(acInstallation, acHost, acPerson, "builds")
	wantCap, _ := projectCapabilityName(acInstallation, "proj-shared")
	if plan.Name != wantName || len(plan.Capabilities) != 1 || plan.CapabilityNames[0] != wantCap || plan.LessPrivileged {
		t.Fatalf("plan %+v", plan)
	}
	for name, c := range map[string]struct {
		owner   JobOwner
		policy  *AccessPolicy
		network networkGrant
		code    string
	}{
		"no policy":       {owner, nil, networkGrant{}, "policy-unavailable"},
		"no context":      {JobOwner{Person: strings.Repeat("f6", 32), Project: "proj-shared"}, policy, networkGrant{}, "context-missing"},
		"another backend": {JobOwner{Person: acOtherPerson, Project: "proj-shared"}, policy, networkGrant{}, "context-backend"},
		"network":         {owner, policy, networkGrant{[]string{"internetClient"}}, "network-not-supported"},
		"bad project":     {JobOwner{Person: acPerson, Project: "../x"}, policy, networkGrant{}, "capability-identity-invalid"},
	} {
		if _, err := planContainer(acInstallation, acHost, c.owner, c.policy, c.network); err == nil || err.Error() != c.code {
			t.Errorf("%s: %v, want %s", name, err, c.code)
		}
	}
}

func TestTheGrantMatrixHoldsTheM2Rules(t *testing.T) {
	if err := validateGrantMatrix(appContainerGrants); err != nil {
		t.Fatal(err)
	}
	mutate := func(change func([]containedGrant) []containedGrant) []containedGrant {
		copy_ := append([]containedGrant{}, appContainerGrants...)
		return change(copy_)
	}
	for name, grants := range map[string][]containedGrant{
		"all application packages on the generation": mutate(func(g []containedGrant) []containedGrant {
			return append(g, containedGrant{roleGeneration, principalAllPackage, rightsReadExec, true, false})
		}),
		"restricted packages anywhere": mutate(func(g []containedGrant) []containedGrant {
			return append(g, containedGrant{roleJobHome, principalRestricted, rightsReadExec, true, false})
		}),
		"package on the executor root": mutate(func(g []containedGrant) []containedGrant {
			return append(g, containedGrant{roleExecutorRoot, principalPackage, rightsTraverse, false, false})
		}),
		"project on the operator folder": mutate(func(g []containedGrant) []containedGrant {
			return append(g, containedGrant{roleOperatorFolder, principalProject, rightsReadExec, false, false})
		}),
		"write to the generation": mutate(func(g []containedGrant) []containedGrant {
			g[0].Rights |= windows.FILE_WRITE_DATA
			return g
		}),
		"job home without a Low label": mutate(func(g []containedGrant) []containedGrant {
			g[3].LowLabel = false
			return g
		}),
		"re-permission the out folder": mutate(func(g []containedGrant) []containedGrant {
			g[5].Rights |= windows.WRITE_DAC
			return g
		}),
		"parent more than traverse": mutate(func(g []containedGrant) []containedGrant {
			g[2].Rights |= windows.FILE_LIST_DIRECTORY
			return g
		}),
		"another job's folder": mutate(func(g []containedGrant) []containedGrant {
			return append(g, containedGrant{roleOtherJob, principalPackage, rightsReadExec, true, false})
		}),
	} {
		if err := validateGrantMatrix(grants); err == nil {
			t.Errorf("%s accepted", name)
		}
	}
	plan := containerPlan{}
	plan.SID, _ = appContainerSID("Zerus.AC.test")
	capability, _ := capabilitySID("zerusProjecttest")
	plan.Capabilities = []*windows.SID{capability}
	for _, g := range appContainerGrants {
		dacl, label, err := grantSDDL(g, plan)
		if err != nil {
			t.Fatal(err)
		}
		sddl := "D:P" + dacl
		if label != "" {
			sddl += "S:" + label
		}
		if _, err := windows.SecurityDescriptorFromString(sddl); err != nil {
			t.Fatalf("%s: %s: %v", g.Role, sddl, err)
		}
	}
}

// Before resume, an uncontained token is refused; there is no fallback.
func TestAnUncontainedTokenIsRefused(t *testing.T) {
	var token windows.Token
	if err := windows.OpenProcessToken(windows.CurrentProcess(), windows.TOKEN_QUERY, &token); err != nil {
		t.Fatal(err)
	}
	defer token.Close()
	sid, _ := appContainerSID("Zerus.AC.test")
	if err := verifyContainedToken(token, containerPlan{SID: sid}); err == nil || err.Error() != "not-appcontainer" {
		t.Fatalf("own token: %v", err)
	}
	if err := verifyContainedToken(token, containerPlan{}); err == nil || err.Error() != "container-plan-invalid" {
		t.Fatalf("empty plan: %v", err)
	}
	if err := verifyContainedToken(token, containerPlan{SID: sid, LessPrivileged: true}); err == nil || err.Error() != "lpac-unverified" {
		t.Fatalf("LPAC: %v", err)
	}
	// The executor's own job membership is not the planned job.
	native, _, _, _ := nativeFixture(t)
	if err := verifyContainedProcess(windows.CurrentProcess(), native.job, containerPlan{SID: sid}); err == nil || err.Error() != "container-job-unverified" {
		t.Fatalf("own process: %v", err)
	}
	// The candidate stays unregistered.
	if requireIsolation(appContainerBackend) == nil {
		t.Fatal("the AppContainer candidate is registered")
	}
}
