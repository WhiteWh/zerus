package main

// The contained probe: `hgs-executor contained-probe` performs a list of
// attempts named in ZERUS_CONTAINED_PROBE and reports each outcome with its
// exact Win32 or Winsock error code. The runtime Qualified() probe and the
// appcontainer-qualify command launch it inside an AppContainer through
// StartContained; uncontained, the same actions establish positive controls.
// It reads nothing outside the targets it is given and prints no file
// contents.

import (
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"runtime"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/registry"
)

const (
	errAccessDenied = 5     // ERROR_ACCESS_DENIED
	wsaAccessDenied = 10013 // WSAEACCES
	wsaTimedOut     = 10060 // WSAETIMEDOUT
	wsaRefused      = 10061 // WSAECONNREFUSED
	maxProbeActions = 64
)

// probeAction is one attempt the probe makes.
type probeAction struct {
	Op     string `json:"op"`
	Target string `json:"target"`
}

// probeOutcome is its result: OK, or the exact error code.
type probeOutcome struct {
	Action probeAction `json:"action"`
	OK     bool        `json:"ok"`
	Code   uint32      `json:"code,omitempty"`
	Detail string      `json:"detail,omitempty"`
}

// denied reports an access refusal by the boundary, not a missing target, an
// absent listener or a timeout.
func (o probeOutcome) denied() bool {
	return !o.OK && (o.Code == errAccessDenied || o.Code == wsaAccessDenied)
}

func errorCode(err error) uint32 {
	var errno syscall.Errno
	if errors.As(err, &errno) {
		return uint32(errno)
	}
	var timeout interface{ Timeout() bool }
	if errors.As(err, &timeout) && timeout.Timeout() {
		return wsaTimedOut
	}
	return 0
}

var procGetHandleInformation = windows.NewLazySystemDLL("kernel32.dll").NewProc("GetHandleInformation")

func getHandleInformation(h windows.Handle, flags *uint32) error {
	if r, _, err := procGetHandleInformation.Call(uintptr(h), uintptr(unsafe.Pointer(flags))); r == 0 {
		return err
	}
	return nil
}

// probeHandleScan finds handles in this process whose disk path contains
// sentinels, process handles to parent, and job handles; inheritableOnly
// limits process and job findings to inheritable handles (the parent's own
// positive control).
func probeHandleScan(sentinels string, parent int, inheritableOnly bool) []string {
	sentinels = strings.ToLower(sentinels)
	var found []string
	for value := uintptr(4); value < 0x10000; value += 4 {
		h := windows.Handle(value)
		var flags uint32
		if getHandleInformation(h, &flags) != nil {
			continue
		}
		inheritable := flags&windows.HANDLE_FLAG_INHERIT != 0
		if t, _ := windows.GetFileType(h); t == windows.FILE_TYPE_DISK && sentinels != "" {
			buffer := make([]uint16, 1024)
			if n, err := windows.GetFinalPathNameByHandle(h, &buffer[0], uint32(len(buffer)), 0); err == nil && int(n) < len(buffer) {
				if strings.Contains(strings.ToLower(windows.UTF16ToString(buffer[:n])), sentinels) {
					found = append(found, "file")
				}
			}
		}
		if inheritableOnly && !inheritable {
			continue
		}
		if pid, err := windows.GetProcessId(h); err == nil && int(pid) == parent {
			found = append(found, "process")
		}
		var accounting jobAccounting
		if windows.QueryInformationJobObject(h, windows.JobObjectBasicAccountingInformation, uintptr(unsafe.Pointer(&accounting)), uint32(unsafe.Sizeof(accounting)), nil) == nil {
			found = append(found, fmt.Sprintf("job %#x", value))
		}
	}
	return found
}

// runProbeAction performs one action in the current process.
func runProbeAction(a probeAction) probeOutcome {
	result := func(err error) probeOutcome {
		if err != nil {
			return probeOutcome{Action: a, Code: errorCode(err), Detail: err.Error()}
		}
		return probeOutcome{Action: a, OK: true}
	}
	switch a.Op {
	case "read":
		f, err := os.Open(a.Target)
		if err == nil {
			_, err = f.Read(make([]byte, 1))
			f.Close()
		}
		return result(err)
	case "write":
		return result(os.WriteFile(a.Target, []byte("probe"), 0600))
	case "list":
		_, err := os.ReadDir(a.Target)
		return result(err)
	case "openprocess":
		pid, _ := strconv.Atoi(a.Target)
		h, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION|windows.PROCESS_DUP_HANDLE, false, uint32(pid))
		if err == nil {
			windows.CloseHandle(h)
		}
		return result(err)
	case "openjob":
		name, _ := windows.UTF16PtrFromString(a.Target)
		proc := windows.NewLazySystemDLL("kernel32.dll").NewProc("OpenJobObjectW")
		h, _, err := proc.Call(0x4|0x1, 0, uintptr(unsafe.Pointer(name))) // JOB_OBJECT_QUERY | JOB_OBJECT_ASSIGN_PROCESS
		if h != 0 {
			windows.CloseHandle(windows.Handle(h))
			return result(nil)
		}
		return result(err)
	case "event":
		name, _ := windows.UTF16PtrFromString(a.Target)
		h, err := windows.OpenEvent(windows.SYNCHRONIZE, false, name)
		if err == nil {
			windows.CloseHandle(h)
		}
		return result(err)
	case "pipe":
		name, _ := windows.UTF16PtrFromString(a.Target)
		h, err := windows.CreateFile(name, windows.GENERIC_READ|windows.GENERIC_WRITE, 0, nil, windows.OPEN_EXISTING, 0, 0)
		if err == nil {
			windows.CloseHandle(h)
		}
		return result(err)
	case "connect":
		c, err := net.DialTimeout("tcp", a.Target, 5*time.Second)
		if err == nil {
			c.Close()
		}
		return result(err)
	case "regread":
		k, err := registry.OpenKey(registry.CURRENT_USER, a.Target, registry.QUERY_VALUE)
		if err == nil {
			k.Close()
		}
		return result(err)
	case "spawn", "spawn-breakaway", "spawn-container", "spawn-capability":
		return result(spawnFromProbe(a))
	case "env":
		return probeOutcome{Action: a, OK: true, Detail: strings.Join(os.Environ(), "\n")}
	case "handles":
		name, pid, _ := strings.Cut(a.Target, "|")
		parent, _ := strconv.Atoi(pid)
		if found := probeHandleScan(name, parent, false); len(found) != 0 {
			return probeOutcome{Action: a, Detail: strings.Join(found, ",")}
		}
		return probeOutcome{Action: a, OK: true}
	case "sleep":
		seconds, _ := strconv.Atoi(a.Target)
		time.Sleep(time.Duration(min(seconds, 600)) * time.Second)
		return probeOutcome{Action: a, OK: true}
	}
	return probeOutcome{Action: a, Detail: "unknown action"}
}

// spawnFromProbe starts this binary again as a no-op probe child: plainly
// (real builds need children), with Job breakaway, in another container, or
// in this container with an extra capability.
func spawnFromProbe(a probeAction) error {
	system, err := systemDirectory()
	if err != nil {
		return err
	}
	cmd := system + `\cmd.exe`
	line, _ := windows.UTF16PtrFromString(windows.ComposeCommandLine([]string{cmd, "/d", "/c", "exit 0"}))
	app, _ := windows.UTF16PtrFromString(cmd)
	env, _ := environmentBlock(containedBaseEnvironment(os.Getenv("TEMP")))
	flags := uint32(windows.CREATE_NO_WINDOW | windows.CREATE_UNICODE_ENVIRONMENT)
	startup := windows.StartupInfoEx{StartupInfo: windows.StartupInfo{Cb: uint32(unsafe.Sizeof(windows.StartupInfoEx{}))}}
	var keep []windows.SIDAndAttributes
	var caps *securityCapabilities
	switch a.Op {
	case "spawn-breakaway":
		flags |= windows.CREATE_BREAKAWAY_FROM_JOB
	case "spawn-container", "spawn-capability":
		plan := containerPlan{}
		if a.Op == "spawn-container" {
			if plan.SID, err = appContainerSID(a.Target); err != nil {
				return err
			}
		} else {
			own, err := currentPackageSID()
			if err != nil {
				return err
			}
			plan.SID = own
			capability, err := capabilitySID(a.Target)
			if err != nil {
				return err
			}
			plan.Capabilities = []*windows.SID{capability}
		}
		attributes, err := windows.NewProcThreadAttributeList(1)
		if err != nil {
			return err
		}
		defer attributes.Delete()
		caps, keep = securityCapabilitiesFor(plan)
		if err := attributes.Update(procThreadAttributeSecurityCapabilities, unsafe.Pointer(caps), unsafe.Sizeof(*caps)); err != nil {
			return err
		}
		startup.ProcThreadAttributeList = attributes.List()
		flags |= windows.EXTENDED_STARTUPINFO_PRESENT
	}
	var process windows.ProcessInformation
	if err := windows.CreateProcess(app, line, nil, nil, false, flags, &env[0], nil, &startup.StartupInfo, &process); err != nil {
		return err
	}
	runtime.KeepAlive(keep)
	runtime.KeepAlive(caps)
	defer windows.CloseHandle(process.Thread)
	defer windows.CloseHandle(process.Process)
	_, _ = windows.WaitForSingleObject(process.Process, 10000)
	if a.Op == "spawn-capability" {
		// Creation may succeed with the capability silently dropped; the
		// child's token decides.
		var token windows.Token
		if err := windows.OpenProcessToken(process.Process, windows.TOKEN_QUERY, &token); err == nil {
			defer token.Close()
			if info, err := tokenInformation(token, tokenCapabilities); err == nil {
				for _, g := range (*windows.Tokengroups)(unsafe.Pointer(&info[0])).AllGroups() {
					if g.Sid.Equals(caps.Capabilities.Sid) {
						return nil
					}
				}
				return syscall.Errno(errAccessDenied)
			}
		}
	}
	return nil
}

// currentPackageSID is the AppContainer SID of this process's token.
func currentPackageSID() (*windows.SID, error) {
	var token windows.Token
	if err := windows.OpenProcessToken(windows.CurrentProcess(), windows.TOKEN_QUERY, &token); err != nil {
		return nil, err
	}
	defer token.Close()
	info, err := tokenInformation(token, tokenAppContainerSid)
	if err != nil {
		return nil, err
	}
	sid := *(**windows.SID)(unsafe.Pointer(&info[0]))
	if sid == nil {
		return nil, errors.New("not in an AppContainer")
	}
	return sid.Copy()
}

// containedProbeMain is the probe child's entry; it prints one line.
func containedProbeMain() int {
	spec := os.Getenv("ZERUS_CONTAINED_PROBE")
	var actions []probeAction
	if err := json.Unmarshal([]byte(spec), &actions); err != nil || len(actions) > maxProbeActions {
		fmt.Println("PROBE-ERROR invalid action list")
		return 2
	}
	outcomes := []probeOutcome{}
	for _, a := range actions {
		outcomes = append(outcomes, runProbeAction(a))
	}
	data, _ := json.Marshal(outcomes)
	fmt.Printf("\nPROBE-OUTCOMES %s\n", data)
	return 0
}

// parseProbeOutcomes reads the probe's line from its captured log.
func parseProbeOutcomes(log []byte) (map[string]probeOutcome, error) {
	text := string(log)
	at := strings.LastIndex(text, "PROBE-OUTCOMES ")
	if at < 0 {
		return nil, errors.New("no probe outcomes")
	}
	line := strings.TrimSpace(strings.SplitN(text[at+len("PROBE-OUTCOMES "):], "\n", 2)[0])
	var outcomes []probeOutcome
	if err := json.Unmarshal([]byte(line), &outcomes); err != nil {
		return nil, err
	}
	out := map[string]probeOutcome{}
	for _, o := range outcomes {
		out[o.Action.Op+" "+o.Action.Target] = o
	}
	return out, nil
}
