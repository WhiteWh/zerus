package main

// The file and capability grant matrix of the AppContainer candidate, as
// data plus a validator. Applying it (DACL entries and mandatory labels) is
// runner-only harness work in this phase; no grant is made on this computer.

import (
	"fmt"

	"golang.org/x/sys/windows"
)

// grantPrincipal names who an entry is for, never a raw SID from a request.
type grantPrincipal string

const (
	principalPackage    grantPrincipal = "package"      // this job's container SID
	principalProject    grantPrincipal = "project"      // the project capability SID
	principalAllPackage grantPrincipal = "all-app"      // ALL APPLICATION PACKAGES, S-1-15-2-1
	principalRestricted grantPrincipal = "all-restrict" // ALL RESTRICTED APPLICATION PACKAGES, S-1-15-2-2
	principalService    grantPrincipal = "service"
	principalSystem     grantPrincipal = "system"
	principalOperator   grantPrincipal = "operator"
)

func (p grantPrincipal) contained() bool {
	return p == principalPackage || p == principalProject || p == principalAllPackage || p == principalRestricted
}

// grantRole names an object in the job's layout.
type grantRole string

const (
	roleGeneration     grantRole = "generation"      // project-slots/<project>/<slot>/build, the leased input
	roleBundle         grantRole = "scenario-bundle" // verified scenario scripts
	roleJobHome        grantRole = "job-home"        // requests/<job>/runtime: HOME, USERPROFILE, APPDATA, LOCALAPPDATA, cwd
	roleJobTemp        grantRole = "job-temp"        // TEMP, TMP (same folder as job-home today)
	roleJobOut         grantRole = "job-out"         // requests/<job>/outputs
	roleJobParent      grantRole = "job-parent"      // requests/<job>
	roleExecutorRoot   grantRole = "executor-root"   // identity, queue, high-water, receipts
	roleOperatorFolder grantRole = "operator-folder" // policy and bootstrap configuration
	roleIdentityKey    grantRole = "identity-key"
	roleOtherJob       grantRole = "other-job" // another person's or context's job folders
	roleHumanProfile   grantRole = "human-profile"
)

const (
	rightsTraverse = windows.FILE_TRAVERSE | windows.SYNCHRONIZE | windows.FILE_READ_ATTRIBUTES
	rightsReadExec = 0x1200a9                      // FILE_GENERIC_READ | FILE_GENERIC_EXECUTE
	rightsModify   = 0x1301bf &^ windows.WRITE_DAC // modify, never re-permission
	rightsWriteAny = windows.FILE_WRITE_DATA | windows.FILE_APPEND_DATA | windows.FILE_WRITE_EA | windows.FILE_WRITE_ATTRIBUTES |
		windows.DELETE | windows.WRITE_DAC | windows.WRITE_OWNER | fileDeleteChild | genericWrite | genericAll
)

type containedGrant struct {
	Role      grantRole
	Principal grantPrincipal
	Rights    uint32
	Inherit   bool // object and container inherit to the role's subtree
	LowLabel  bool // mandatory label Low, no-write-up, inherited
}

// appContainerGrants is the complete matrix of contained grants. Every role
// not listed gets nothing from any contained principal.
var appContainerGrants = []containedGrant{
	{roleGeneration, principalProject, rightsReadExec, true, false},
	{roleBundle, principalProject, rightsReadExec, true, false},
	{roleJobParent, principalPackage, rightsTraverse, false, false},
	{roleJobHome, principalPackage, rightsModify, true, true},
	{roleJobTemp, principalPackage, rightsModify, true, true},
	{roleJobOut, principalPackage, rightsModify, true, true},
}

// forbiddenRoles must hold no contained entry at all, explicit or inherited.
var forbiddenRoles = map[grantRole]bool{
	roleExecutorRoot: true, roleOperatorFolder: true, roleIdentityKey: true, roleOtherJob: true, roleHumanProfile: true,
}

// validateGrantMatrix refuses a matrix that would give a contained principal
// more than the M2 rules: no ALL APPLICATION PACKAGES or ALL RESTRICTED
// APPLICATION PACKAGES grant anywhere; nothing on executor, operator, key,
// other-job or human-profile objects; project inputs read and execute only;
// writes only by the job's own package SID on its own job folders, each with
// a Low label; its parent traverse only; never re-permission or re-own.
func validateGrantMatrix(grants []containedGrant) error {
	seen := map[string]bool{}
	for _, g := range grants {
		key := string(g.Role) + "/" + string(g.Principal)
		if seen[key] {
			return fmt.Errorf("duplicate grant %s", key)
		}
		seen[key] = true
		if !g.Principal.contained() {
			continue
		}
		switch {
		case g.Principal == principalAllPackage || g.Principal == principalRestricted:
			return fmt.Errorf("%s: no grant to all application packages", g.Role)
		case forbiddenRoles[g.Role]:
			return fmt.Errorf("%s: no contained grant", g.Role)
		case g.Rights&(windows.WRITE_DAC|windows.WRITE_OWNER|genericAll) != 0:
			return fmt.Errorf("%s: contained principals never re-permission or re-own", g.Role)
		}
		switch g.Role {
		case roleGeneration, roleBundle:
			if g.Principal != principalProject || g.Rights&^rightsReadExec != 0 || g.LowLabel {
				return fmt.Errorf("%s: project capability read and execute only", g.Role)
			}
		case roleJobHome, roleJobTemp, roleJobOut:
			if g.Principal != principalPackage || !g.LowLabel || !g.Inherit {
				return fmt.Errorf("%s: written only by the job's own package SID with a Low label", g.Role)
			}
		case roleJobParent:
			if g.Principal != principalPackage || g.Rights&^rightsTraverse != 0 || g.Inherit {
				return fmt.Errorf("%s: traverse only, not inherited", g.Role)
			}
		default:
			return fmt.Errorf("%s: unknown role", g.Role)
		}
	}
	return nil
}

// grantSDDL is the DACL entry, and for writable roles the label, that the
// runner harness applies for one plan.
func grantSDDL(g containedGrant, plan containerPlan) (dacl, label string, err error) {
	var sid string
	switch g.Principal {
	case principalPackage:
		sid = plan.SID.String()
	case principalProject:
		if len(plan.Capabilities) != 1 {
			return "", "", fmt.Errorf("plan needs exactly one project capability")
		}
		sid = plan.Capabilities[0].String()
	default:
		return "", "", fmt.Errorf("%s is not applied by the launcher", g.Principal)
	}
	flags := ""
	if g.Inherit {
		flags = "OICI"
	}
	dacl = fmt.Sprintf("(A;%s;0x%x;;;%s)", flags, g.Rights, sid)
	if g.LowLabel {
		label = "(ML;OICI;NW;;;LW)"
	}
	return dacl, label, nil
}
