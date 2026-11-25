package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"sort"
	"strings"
	"sync"
	"time"
	"unicode/utf16"
	"unsafe"

	"golang.org/x/sys/windows"
)

const maxScenarioLog = 8 * MiB

type processReceipt struct {
	Name     string `json:"kernel_job"`
	PID      uint32 `json:"pid"`
	Birth    uint64 `json:"birth"`
	Accepted bool   `json:"creation_accepted"`
	Issued   bool   `json:"creation_issued"`
}

type capture struct {
	mu             sync.Mutex
	file           *os.File
	bytes, omitted int64
	err            error
}

func (c *capture) Write(b []byte) (int, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	n := len(b)
	keep := min(int64(n), maxScenarioLog-c.bytes)
	if keep > 0 && c.err == nil {
		written, err := c.file.Write(b[:keep])
		c.bytes += int64(written)
		if err != nil {
			c.err = err
		}
	}
	c.omitted += int64(n) - keep
	return n, nil
}

type nativeScenario struct {
	job     windows.Handle
	process windows.Handle
	thread  windows.Handle
	info    processReceipt
	pipe    *os.File
	capture *capture
	drained chan struct{}
}

func newNativeScenario(memory int64) (*nativeScenario, error) {
	if memory < 64*MiB || memory > 16*1024*MiB {
		return nil, errors.New("scenario memory admission refused")
	}
	name, err := newIdentity()
	if err != nil {
		return nil, err
	}
	name = `Local\ZerusScenario-` + name
	ptr, err := windows.UTF16PtrFromString(name)
	if err != nil {
		return nil, err
	}
	job, err := windows.CreateJobObject(nil, ptr)
	if err != nil {
		return nil, err
	}
	limits := windows.JOBOBJECT_EXTENDED_LIMIT_INFORMATION{}
	limits.BasicLimitInformation.LimitFlags = windows.JOB_OBJECT_LIMIT_PROCESS_MEMORY | windows.JOB_OBJECT_LIMIT_JOB_MEMORY |
		windows.JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | windows.JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION | windows.JOB_OBJECT_LIMIT_ACTIVE_PROCESS
	limits.BasicLimitInformation.ActiveProcessLimit = 64
	limits.ProcessMemoryLimit = uintptr(memory)
	limits.JobMemoryLimit = uintptr(memory)
	if _, err := windows.SetInformationJobObject(job, windows.JobObjectExtendedLimitInformation, uintptr(unsafe.Pointer(&limits)), uint32(unsafe.Sizeof(limits))); err != nil {
		windows.CloseHandle(job)
		return nil, err
	}
	return &nativeScenario{job: job, info: processReceipt{Name: name}}, nil
}

func (j *nativeScenario) Close() {
	if j.thread != 0 {
		windows.CloseHandle(j.thread)
		j.thread = 0
	}
	if j.process != 0 {
		windows.CloseHandle(j.process)
		j.process = 0
	}
	if j.job != 0 {
		windows.CloseHandle(j.job)
		j.job = 0
	}
	if j.pipe != nil {
		j.pipe.Close()
	}
}

func environmentBlock(values map[string]string) ([]uint16, error) {
	keys := make([]string, 0, len(values))
	for key, value := range values {
		if key == "" || strings.ContainsAny(key, "=\x00") || strings.ContainsRune(value, 0) {
			return nil, errors.New("invalid scenario environment")
		}
		keys = append(keys, key)
	}
	sort.Strings(keys)
	var block []uint16
	for _, key := range keys {
		block = append(block, utf16.Encode([]rune(key+"="+values[key]))...)
		block = append(block, 0)
	}
	block = append(block, 0)
	if len(block) == 1 {
		block = append(block, 0)
	}
	return block, nil
}

func (j *nativeScenario) Start(executable string, args []string, environment map[string]string, cwd string, log *os.File, created func(processReceipt) error) error {
	app, err := windows.UTF16PtrFromString(executable)
	if err != nil {
		return err
	}
	line, err := windows.UTF16PtrFromString(windows.ComposeCommandLine(append([]string{executable}, args...)))
	if err != nil {
		return err
	}
	directory, err := windows.UTF16PtrFromString(cwd)
	if err != nil {
		return err
	}
	env, err := environmentBlock(environment)
	if err != nil {
		return err
	}
	reader, writer, err := os.Pipe()
	if err != nil {
		return err
	}
	defer writer.Close()
	j.pipe = reader
	null, err := os.Open(os.DevNull)
	if err != nil {
		return err
	}
	defer null.Close()
	attributes, err := windows.NewProcThreadAttributeList(2)
	if err != nil {
		return err
	}
	defer attributes.Delete()
	handles := []windows.Handle{windows.Handle(writer.Fd()), windows.Handle(null.Fd())}
	for _, h := range handles {
		if err := windows.SetHandleInformation(h, windows.HANDLE_FLAG_INHERIT, windows.HANDLE_FLAG_INHERIT); err != nil {
			return err
		}
	}
	if err := attributes.Update(windows.PROC_THREAD_ATTRIBUTE_HANDLE_LIST, unsafe.Pointer(&handles[0]), uintptr(len(handles))*unsafe.Sizeof(handles[0])); err != nil {
		return err
	}
	jobs := []windows.Handle{j.job}
	// Creation-time job association (Windows 10+); no post-start assignment
	// fallback. The thread stays suspended until its exact identity is durable.
	// https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute
	if err := attributes.Update(0x2000d, unsafe.Pointer(&jobs[0]), unsafe.Sizeof(jobs[0])); err != nil {
		return err
	}
	startup := windows.StartupInfoEx{StartupInfo: windows.StartupInfo{Cb: uint32(unsafe.Sizeof(windows.StartupInfoEx{})),
		Flags: windows.STARTF_USESTDHANDLES | windows.STARTF_USESHOWWINDOW, ShowWindow: windows.SW_HIDE,
		StdInput: handles[1], StdOutput: handles[0], StdErr: handles[0]}, ProcThreadAttributeList: attributes.List()}
	var process windows.ProcessInformation
	flags := uint32(windows.CREATE_SUSPENDED | windows.CREATE_UNICODE_ENVIRONMENT | windows.EXTENDED_STARTUPINFO_PRESENT | windows.CREATE_NO_WINDOW | windows.CREATE_DEFAULT_ERROR_MODE)
	j.info.Issued = true
	if err := created(j.info); err != nil {
		return err
	}
	if err := windows.CreateProcess(app, line, nil, nil, true, flags, &env[0], directory, &startup.StartupInfo, &process); err != nil {
		return err
	}
	j.process, j.thread = process.Process, process.Thread
	var creation, exit, kernel, user windows.Filetime
	if err := windows.GetProcessTimes(process.Process, &creation, &exit, &kernel, &user); err != nil {
		return err
	}
	belongs, checkErr := processInJob(process.Process, j.job)
	if checkErr != nil || !belongs {
		_ = windows.TerminateProcess(process.Process, 0xe0000001)
		_, _ = windows.WaitForSingleObject(process.Process, 10000)
		return errors.New("scenario process custody could not be verified")
	}
	j.info = processReceipt{Name: j.info.Name, PID: process.ProcessId, Birth: uint64(creation.HighDateTime)<<32 | uint64(creation.LowDateTime), Accepted: true, Issued: true}
	if err := created(j.info); err != nil {
		return err
	}
	j.capture = &capture{file: log}
	j.drained = make(chan struct{})
	go func() { _, _ = io.Copy(j.capture, reader); close(j.drained) }()
	if _, err := windows.ResumeThread(process.Thread); err != nil {
		return err
	}
	return nil
}

func processInJob(process, job windows.Handle) (bool, error) {
	proc := windows.NewLazySystemDLL("kernel32.dll").NewProc("IsProcessInJob")
	if err := proc.Find(); err != nil {
		return false, err
	}
	var member uint32
	result, _, err := proc.Call(uintptr(process), uintptr(job), uintptr(unsafe.Pointer(&member)))
	if result == 0 {
		return false, fmt.Errorf("cannot verify process job membership: %w", err)
	}
	return member != 0, nil
}

type jobAccounting struct {
	TotalUserTime, TotalKernelTime, ThisPeriodUserTime, ThisPeriodKernelTime int64
	PageFaults, TotalProcesses, ActiveProcesses, TerminatedProcesses         uint32
}

func jobActive(job windows.Handle) (uint32, error) {
	var accounting jobAccounting
	err := windows.QueryInformationJobObject(job, windows.JobObjectBasicAccountingInformation, uintptr(unsafe.Pointer(&accounting)), uint32(unsafe.Sizeof(accounting)), nil)
	return accounting.ActiveProcesses, err
}

func (j *nativeScenario) Wait(seconds int, tick func() error) (uint32, bool, error) {
	deadline := time.Now().Add(time.Duration(seconds) * time.Second)
	timedOut := false
	for {
		if tick != nil {
			if err := tick(); err != nil {
				return 0, false, err
			}
		}
		active, err := jobActive(j.job)
		if err != nil {
			return 0, false, err
		}
		if active == 0 {
			break
		}
		if time.Now().After(deadline) {
			timedOut = true
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	if timedOut {
		if err := windows.TerminateJobObject(j.job, 0xe0000001); err != nil {
			return 0, true, err
		}
	}
	settle := time.Now().Add(10 * time.Second)
	for {
		active, err := jobActive(j.job)
		if err != nil {
			return 0, timedOut, err
		}
		if active == 0 {
			break
		}
		if time.Now().After(settle) {
			return 0, timedOut, errors.New("scenario child settlement is unknown")
		}
		time.Sleep(10 * time.Millisecond)
	}
	if status, err := windows.WaitForSingleObject(j.process, 1000); err != nil || status != windows.WAIT_OBJECT_0 {
		return 0, timedOut, errors.New("scenario root exit is unknown")
	}
	var code uint32
	if err := windows.GetExitCodeProcess(j.process, &code); err != nil {
		return 0, timedOut, err
	}
	if j.drained != nil {
		select {
		case <-j.drained:
		case <-time.After(2 * time.Second):
			j.pipe.Close()
			return code, timedOut, errors.New("scenario log settlement is unknown")
		}
		j.capture.mu.Lock()
		err := j.capture.err
		j.capture.mu.Unlock()
		if err != nil {
			return code, timedOut, err
		}
	}
	return code, timedOut, nil
}

func rootExited(info processReceipt) (bool, error) {
	if !info.Accepted || info.PID == 0 || info.Birth == 0 {
		return false, errors.New("process creation outcome is unknown")
	}
	h, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION|windows.SYNCHRONIZE, false, info.PID)
	if err == windows.ERROR_INVALID_PARAMETER {
		return true, nil
	}
	if err != nil {
		return false, err
	}
	defer windows.CloseHandle(h)
	var birth, exit, kernel, user windows.Filetime
	if err := windows.GetProcessTimes(h, &birth, &exit, &kernel, &user); err != nil {
		return false, err
	}
	if uint64(birth.HighDateTime)<<32|uint64(birth.LowDateTime) != info.Birth {
		return true, nil
	}
	status, err := windows.WaitForSingleObject(h, 0)
	return status == windows.WAIT_OBJECT_0, err
}

func namedJobSettled(info processReceipt) (bool, error) {
	if !info.Accepted && !info.Issued {
		return true, nil
	}
	exited, err := rootExited(info)
	if err != nil || !exited {
		return false, err
	}
	name, err := windows.UTF16PtrFromString(info.Name)
	if err != nil {
		return false, err
	}
	proc := windows.NewLazySystemDLL("kernel32.dll").NewProc("OpenJobObjectW")
	if err := proc.Find(); err != nil {
		return false, err
	}
	h, _, callErr := proc.Call(4, 0, uintptr(unsafe.Pointer(name))) // JOB_OBJECT_QUERY
	if h == 0 {
		if callErr == windows.ERROR_FILE_NOT_FOUND {
			return true, nil
		}
		return false, fmt.Errorf("cannot inspect owned scenario job: %w", callErr)
	}
	defer windows.CloseHandle(windows.Handle(h))
	active, err := jobActive(windows.Handle(h))
	return active == 0, err
}

func systemDirectory() (string, error) { return windows.GetSystemDirectory() }

func currentSupervisor() (processReceipt, error) {
	var born, exit, kernel, user windows.Filetime
	if err := windows.GetProcessTimes(windows.CurrentProcess(), &born, &exit, &kernel, &user); err != nil {
		return processReceipt{}, err
	}
	return processReceipt{PID: uint32(os.Getpid()), Birth: uint64(born.HighDateTime)<<32 | uint64(born.LowDateTime), Accepted: true}, nil
}

func readSnapshot(path string, value any) error {
	f, err := nativeOpen(path, windows.GENERIC_READ, windows.FILE_SHARE_READ|windows.FILE_SHARE_DELETE, windows.OPEN_EXISTING, 0)
	if err != nil {
		return err
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil || !info.Mode().IsRegular() || info.Size() > maxTreeBytes {
		return errors.New("snapshot record size or type refused")
	}
	b, err := io.ReadAll(io.LimitReader(f, maxTreeBytes+1))
	if err != nil || int64(len(b)) > maxTreeBytes {
		return errors.New("snapshot read refused")
	}
	decoder := json.NewDecoder(strings.NewReader(string(b)))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(value); err != nil {
		return errors.New("invalid snapshot")
	}
	if decoder.Decode(new(any)) != io.EOF {
		return errors.New("trailing snapshot data")
	}
	return nil
}

func statEvidence(path string) (*os.File, error) {
	return nativeOpen(path, windows.FILE_READ_ATTRIBUTES, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE|windows.FILE_SHARE_DELETE, windows.OPEN_EXISTING, 0)
}

func openLiveLog(path string) (*os.File, error) {
	return nativeOpen(path, windows.GENERIC_READ, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE|windows.FILE_SHARE_DELETE, windows.OPEN_EXISTING, 0)
}

func limitExecutorProcess(memory int64) (func(), error) {
	job, err := windows.CreateJobObject(nil, nil)
	if err != nil {
		return nil, err
	}
	limits := windows.JOBOBJECT_EXTENDED_LIMIT_INFORMATION{}
	limits.BasicLimitInformation.LimitFlags = windows.JOB_OBJECT_LIMIT_PROCESS_MEMORY | windows.JOB_OBJECT_LIMIT_JOB_MEMORY | windows.JOB_OBJECT_LIMIT_ACTIVE_PROCESS | windows.JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
	limits.BasicLimitInformation.ActiveProcessLimit = 128
	limits.ProcessMemoryLimit = uintptr(memory)
	limits.JobMemoryLimit = uintptr(memory)
	if _, err := windows.SetInformationJobObject(job, windows.JobObjectExtendedLimitInformation, uintptr(unsafe.Pointer(&limits)), uint32(unsafe.Sizeof(limits))); err != nil {
		windows.CloseHandle(job)
		return nil, err
	}
	if err := windows.AssignProcessToJobObject(job, windows.CurrentProcess()); err != nil {
		windows.CloseHandle(job)
		return nil, fmt.Errorf("executor memory job assignment: %w", err)
	}
	return func() { windows.CloseHandle(job) }, nil
}
