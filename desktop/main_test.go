package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestBackendCommandPrefersSidecar(t *testing.T) {
	root := t.TempDir()
	path := sidecarPath(root)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("stub"), 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("VELMIRA_PYTHON", "")
	old := *flagPython
	*flagPython = ""
	t.Cleanup(func() { *flagPython = old })
	cmd, err := backendCommand(root, 8876)
	if err != nil {
		t.Fatal(err)
	}
	if cmd.Path != path || len(cmd.Args) != 5 || cmd.Args[4] != "8876" {
		t.Fatalf("expected embedded sidecar and port, got %v", cmd.Args)
	}
}

func TestBackendCommandHonorsExplicitPython(t *testing.T) {
	executable, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	old := *flagPython
	*flagPython = executable
	t.Cleanup(func() { *flagPython = old })
	cmd, err := backendCommand(t.TempDir(), 8877)
	if err != nil {
		t.Fatal(err)
	}
	if cmd.Path != executable || len(cmd.Args) < 4 || cmd.Args[2] != "uvicorn" {
		t.Fatalf("expected explicit Python source backend, got %v", cmd.Args)
	}
}

func TestBundledDataDirectoryUsesUserCache(t *testing.T) {
	t.Setenv("DATA_DIR", "")
	root := t.TempDir()
	path := sidecarPath(root)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, nil, 0o755); err != nil {
		t.Fatal(err)
	}
	cache, err := os.UserCacheDir()
	if err != nil {
		t.Skip(err)
	}
	if got := dataDir(root); got != filepath.Join(cache, "Velmira", "data") {
		t.Fatalf("unexpected data path %q", got)
	}
	if err := os.MkdirAll(filepath.Join(root, "server", "app"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "server", "app", "main.py"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	if got := dataDir(root); got != filepath.Join(root, "data") {
		t.Fatalf("source checkout library moved unexpectedly: %q", got)
	}
	t.Setenv("DATA_DIR", filepath.Join(root, "custom"))
	if got := dataDir(root); got != filepath.Join(root, "custom") {
		t.Fatalf("DATA_DIR override ignored: %q", got)
	}
}
