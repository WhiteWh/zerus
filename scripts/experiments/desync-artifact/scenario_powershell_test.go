package main

import (
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func TestPowershellScenarioCommandQuotesEveryArgument(t *testing.T) {
	got := powershellScenarioCommand(`C:\Windows\System32`, `C:\b\it's\run.ps1`, []string{"a b", "x\u2019y", ""})
	want := `$env:PSModulePath = 'C:\Windows\System32\WindowsPowerShell\v1.0\Modules'; $global:LASTEXITCODE = 0; & 'C:\b\it''s\run.ps1' 'a b' 'x’’y' ''; exit $LASTEXITCODE`
	if got != want {
		t.Fatalf("got  %s\nwant %s", got, want)
	}
}

// The command runs a script with its arguments intact, keeps its exit code
// and leaves only Windows' own modules on the path, with autoloading working.
func TestPowershellScenarioCommandRunsTheScript(t *testing.T) {
	if runtime.GOOS != "windows" {
		t.Skip("Windows PowerShell")
	}
	system, err := systemDirectory()
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	script := filepath.Join(dir, "it's.ps1")
	body := "param($a, $b)\n\"$a|$b|$env:PSModulePath|$(Join-Path 'x' 'y')\"\nexit 7\n"
	if err := os.WriteFile(script, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(filepath.Join(system, "WindowsPowerShell", "v1.0", "powershell.exe"), "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command",
		powershellScenarioCommand(system, script, []string{"one two", "it's"}))
	out, err := cmd.CombinedOutput()
	exit, ok := err.(*exec.ExitError)
	if !ok || exit.ExitCode() != 7 {
		t.Fatalf("exit %v: %s", err, out)
	}
	want := "one two|it's|" + filepath.Join(system, "WindowsPowerShell", "v1.0", "Modules") + `|x\y`
	if strings.TrimSpace(string(out)) != want {
		t.Fatalf("got %s want %s", out, want)
	}
}
