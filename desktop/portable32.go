//go:build portable32

// A small 32-bit fallback launcher. MyGo's native window backend supports
// only amd64 and arm64, so 32-bit distributions open the same local UI in
// the operating system browser while keeping the Python backend local.
package main

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"runtime"
	"strconv"
	"syscall"
	"time"
)

func main() {
	root := findPortableRoot()
	port := portablePort()
	cmd, err := portableBackend(root, port)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return
	}
	cmd.Dir = root
	cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
	cmd.Env = append(os.Environ(), "PYTHONIOENCODING=utf-8")
	if os.Getenv("DATA_DIR") == "" {
		if directory, configErr := os.UserConfigDir(); configErr == nil {
			cmd.Env = append(cmd.Env, "DATA_DIR="+filepath.Join(directory, "Velmira", "data"))
		}
	}
	if err := cmd.Start(); err != nil {
		fmt.Fprintln(os.Stderr, "启动 Velmira 后端失败：", err)
		return
	}
	defer killProcessTree(cmd.Process.Pid)
	exited := make(chan error, 1)
	go func() { exited <- cmd.Wait() }()
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt, syscall.SIGTERM)
	defer signal.Stop(stop)

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	url := "http://127.0.0.1:" + strconv.Itoa(port)
	for {
		request, _ := http.NewRequestWithContext(ctx, http.MethodGet, url+"/api/health", nil)
		response, requestErr := http.DefaultClient.Do(request)
		if requestErr == nil && response.StatusCode == http.StatusOK {
			response.Body.Close()
			openPortableBrowser(url)
			fmt.Println("Velmira:", url, "\nKeep this launcher open. Press Ctrl+C to stop the local server.")
			break
		}
		if response != nil {
			response.Body.Close()
		}
		select {
		case <-ctx.Done():
			fmt.Fprintln(os.Stderr, "Velmira 后端启动超时")
			return
		case <-stop:
			return
		case err := <-exited:
			fmt.Fprintln(os.Stderr, "Velmira 后端退出：", err)
			return
		case <-time.After(100 * time.Millisecond):
		}
	}
	select {
	case <-stop:
	case <-exited:
	}
}

func findPortableRoot() string {
	if value := os.Getenv("VELMIRA_ROOT"); value != "" {
		return value
	}
	executable, _ := os.Executable()
	root := filepath.Dir(executable)
	if _, err := os.Stat(filepath.Join(root, "server", "app", "main.py")); err == nil {
		return root
	}
	return "."
}

func portablePort() int {
	if value := os.Getenv("VELMIRA_PORT"); value != "" {
		if port, err := strconv.Atoi(value); err == nil && port > 0 {
			return port
		}
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err == nil {
		port := listener.Addr().(*net.TCPAddr).Port
		listener.Close()
		return port
	}
	return 8765
}

func portableBackend(root string, port int) (*exec.Cmd, error) {
	python := os.Getenv("VELMIRA_PYTHON")
	if python == "" {
		python = "python3"
		if runtime.GOOS == "windows" {
			python = "python"
		}
	}
	return exec.Command(python, "-m", "uvicorn", "server.app.main:app", "--host", "127.0.0.1", "--port", strconv.Itoa(port), "--no-proxy-headers"), nil
}

func openPortableBrowser(url string) {
	var command *exec.Cmd
	if runtime.GOOS == "windows" {
		command = exec.Command("rundll32", "url.dll,FileProtocolHandler", url)
	}
	if runtime.GOOS == "linux" {
		command = exec.Command("xdg-open", url)
	}
	if command != nil {
		_ = command.Start()
	}
}
