package main

import (
	"context"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"
)

func TestDirectorySidecarWinsOverLegacySingleFile(t *testing.T) {
	root := t.TempDir()
	directory := sidecarPath(root)
	if err := os.MkdirAll(filepath.Dir(directory), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(directory, nil, 0o755); err != nil {
		t.Fatal(err)
	}
	legacy := filepath.Join(root, "backend", filepath.Base(directory))
	if err := os.WriteFile(legacy, nil, 0o755); err != nil {
		t.Fatal(err)
	}
	if got := sidecarPath(root); got != directory {
		t.Fatalf("legacy extracting backend selected: %s", got)
	}
	if err := os.Remove(directory); err != nil {
		t.Fatal(err)
	}
	if got := sidecarPath(root); got != legacy {
		t.Fatalf("legacy fallback unavailable: %s", got)
	}
}

func TestReuseBackendDoesNotStopExternalServer(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/api/health" {
			t.Errorf("unexpected path %s", r.URL.Path)
		}
		w.Write([]byte(`{"ok":true,"name":"Velmira"}`))
	}))
	defer server.Close()
	_, portText, _ := net.SplitHostPort(server.Listener.Addr().String())
	port, _ := strconv.Atoi(portText)
	url, stop, wait, err := startBackend(t.TempDir(), port)
	if err != nil {
		t.Fatal(err)
	}
	if url != server.URL {
		t.Fatalf("external server not reused: %s", url)
	}
	if err := wait(context.Background()); err != nil {
		t.Fatal(err)
	}
	stop()
	if _, ok := healthAt(context.Background(), port); !ok {
		t.Fatal("external service was stopped")
	}
}

func TestHealthProbeIsBoundedAndCancellable(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { <-r.Context().Done() }))
	defer server.Close()
	_, portText, _ := net.SplitHostPort(server.Listener.Addr().String())
	port, _ := strconv.Atoi(portText)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	started := time.Now()
	if _, ok := healthAt(ctx, port); ok {
		t.Fatal("cancelled probe succeeded")
	}
	probeCtx, probeCancel := context.WithTimeout(context.Background(), 150*time.Millisecond)
	defer probeCancel()
	if _, ok := healthAt(probeCtx, port); ok {
		t.Fatal("unresponsive service succeeded")
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Fatalf("probe stalled startup: %v", elapsed)
	}
}

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
