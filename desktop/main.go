//go:build !portable32

// Velmira desktop — a MyGo app that launches the local Velmira server
// (Python + FastAPI) and shows its web UI in a native window.
//
//	cd desktop && go build -trimpath -ldflags '-H=windowsgui -s -w' -o Velmira.exe .
//	./Velmira.exe                      # normal desktop app
//	./Velmira.exe -screenshot shot.png # capture the window and quit (CI / evidence)
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/egoist/mygo"
)

var (
	flagScreenshot = flag.String("screenshot", "", "capture the window to this PNG file after loading, then quit")
	flagWait       = flag.Duration("wait", 5*time.Second, "how long to let the page render before -screenshot")
	flagPort       = flag.Int("port", 0, "backend port (0 = VELMIRA_PORT or auto)")
	flagRoot       = flag.String("root", "", "project root (default: found from cwd / executable)")
	flagPython     = flag.String("python", "", "python executable (default: VELMIRA_PYTHON or python)")
	flagWidth      = flag.Int("width", 1280, "window width in DIPs")
	flagHeight     = flag.Int("height", 860, "window height in DIPs")
	flagBind       = flag.String("bind", "127.0.0.1", "backend bind address (0.0.0.0 to allow phones on the LAN)")
)

type health struct {
	OK      bool   `json:"ok"`
	Name    string `json:"name"`
	Version string `json:"version"`
}

func main() {
	flag.Parse()

	root, err := findRoot()
	if err != nil {
		log.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	baseURL, stop, wait, err := startBackend(root, backendPort())
	if err != nil {
		showFatal(err)
		return
	}
	defer stop()
	mygo.App.OnQuit(func() { cancel(); stop() })
	ready := make(chan error, 1)
	go func() { ready <- wait(ctx) }()
	// The desktop shell has no native menu; all user actions live in the web UI.
	mygo.App.SetMenu(nil)

	mygo.App.WhenReady(func() {
		win := mygo.NewWindow(mygo.WindowOptions{
			Title:           "Velmira",
			Width:           *flagWidth,
			Height:          *flagHeight,
			MinWidth:        900,
			MinHeight:       600,
			BackgroundColor: "light-dark(#f6f3ec, #171614)",
			// The page is our own local server; trust it for typed IPC.
			Page: mygo.PageOptions{TrustedOrigins: []string{baseURL}},
		})
		win.Page().OnContextMenu(func(e *mygo.ContextMenuEvent) {
			e.PreventDefault()
		})
		// WebView2 and the backend initialize in parallel. The same window remains
		// responsive during startup and displays any error without opening another.
		win.Page().LoadHTML(startupHTML, "")
		go func() {
			if err := <-ready; err != nil {
				if ctx.Err() == nil {
					win.Page().LoadHTML(fatalHTML(err), "")
				}
				return
			}
			if ctx.Err() != nil {
				return
			}
			if err := win.Page().LoadURL(baseURL + "/"); err != nil {
				win.Page().LoadHTML(fatalHTML(err), "")
				return
			}
			if *flagScreenshot != "" {
				// LoadURL queues navigation; capture only once the shelf is mounted.
				captureCtx, captureCancel := context.WithTimeout(ctx, 20*time.Second)
				defer captureCancel()
				for captureCtx.Err() == nil {
					value, _ := win.Page().EvalContext(captureCtx, `!!document.querySelector('#library-view:not([hidden])') && document.querySelector('#lib-skeleton')?.hidden`)
					if value == true {
						break
					}
					time.Sleep(50 * time.Millisecond)
				}
				if captureCtx.Err() != nil {
					mygo.App.Quit()
					return
				}
				time.Sleep(*flagWait)
				png, err := win.CapturePage()
				if err != nil {
					log.Printf("capture: %v", err)
				} else if err := os.WriteFile(*flagScreenshot, png, 0o644); err != nil {
					log.Printf("write screenshot: %v", err)
				} else {
					log.Printf("screenshot: %s", *flagScreenshot)
				}
				mygo.App.Quit()
			}
		}()
	})

	if err := mygo.App.Run(); err != nil {
		log.Fatal(err)
	}
}

// ---------- project root, backend process ----------

func findRoot() (string, error) {
	if *flagRoot != "" {
		return filepath.Abs(*flagRoot)
	}
	if v := os.Getenv("VELMIRA_ROOT"); v != "" {
		return filepath.Abs(v)
	}
	var dirs []string
	if wd, err := os.Getwd(); err == nil {
		dirs = append(dirs, wd)
	}
	if exe, err := os.Executable(); err == nil {
		dirs = append([]string{filepath.Dir(exe)}, dirs...)
	}
	for _, d := range dirs {
		for i := 0; i < 6; i++ {
			if _, err := os.Stat(sidecarPath(d)); err == nil {
				return d, nil
			}
			if _, err := os.Stat(filepath.Join(d, "server", "app", "main.py")); err == nil {
				return d, nil
			}
			parent := filepath.Dir(d)
			if parent == d {
				break
			}
			d = parent
		}
	}
	return "", fmt.Errorf("找不到 Velmira 后端（backend/Velmira-backend/、backend/Velmira-backend.exe 或 server/app/main.py）；用 -root 指定")
}

func sidecarPath(root string) string {
	name := "Velmira-backend"
	fileName := name
	if os.PathSeparator == '\\' {
		fileName += ".exe"
	}
	oneFile := filepath.Join(root, "backend", fileName)
	directory := filepath.Join(root, "backend", name, fileName)
	if _, err := os.Stat(directory); err == nil {
		return directory
	}
	// Directory-mode PyInstaller builds avoid extracting the backend on every launch.
	if _, err := os.Stat(oneFile); err == nil {
		return oneFile
	}
	return directory
}

func backendPort() int {
	if *flagPort != 0 {
		return *flagPort
	}
	if v := os.Getenv("VELMIRA_PORT"); v != "" {
		if n, err := strconv.Atoi(v); err == nil {
			return n
		}
	}
	return 8765
}

// Local probes must bypass proxies and promptly retry a port that is not yet open.
var healthClient = &http.Client{Timeout: 1500 * time.Millisecond, Transport: &http.Transport{
	DialContext: (&net.Dialer{Timeout: 100 * time.Millisecond}).DialContext,
}}

func healthAt(ctx context.Context, port int) (health, bool) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("http://127.0.0.1:%d/api/health", port), nil)
	if err != nil {
		return health{}, false
	}
	resp, err := healthClient.Do(req)
	if err != nil {
		return health{}, false
	}
	defer resp.Body.Close()
	if resp.StatusCode != 200 {
		return health{}, false
	}
	var h health
	if err := json.NewDecoder(resp.Body).Decode(&h); err != nil {
		return health{}, false
	}
	return h, h.OK
}

func freePort() int {
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return 8765
	}
	defer l.Close()
	return l.Addr().(*net.TCPAddr).Port
}

func startBackend(root string, port int) (baseURL string, stop func(), wait func(context.Context) error, err error) {
	// A bind check avoids waiting for a failed TCP connection on every fresh launch.
	listener, listenErr := net.Listen("tcp", net.JoinHostPort(*flagBind, strconv.Itoa(port)))
	if listenErr != nil {
		// Probe an occupied port long enough to reuse an existing local server.
		if h, ok := healthAt(context.Background(), port); ok && h.Name == "Velmira" {
			return fmt.Sprintf("http://127.0.0.1:%d", port), func() {}, func(context.Context) error { return nil }, nil
		}
		port = freePort()
	} else {
		listener.Close()
	}

	cmd, err := backendCommand(root, port)
	if err != nil {
		return "", nil, nil, err
	}

	logPath := backendLogPath()
	rotateBackendLog(logPath, 5<<20, 3)
	logFile, _ := os.OpenFile(logPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if logFile != nil {
		fmt.Fprintf(logFile, "\n=== %s velmira backend :%d ===\n", time.Now().Format(time.RFC3339), port)
	}
	cmd.Dir = root
	cmd.Env = append(os.Environ(), "PYTHONIOENCODING=utf-8")
	if os.Getenv("DATA_DIR") == "" {
		cmd.Env = append(cmd.Env, "DATA_DIR="+dataDir(root))
	}
	if logFile != nil {
		cmd.Stdout, cmd.Stderr = logFile, logFile
	}
	hideWindow(cmd)
	if err := cmd.Start(); err != nil {
		if logFile != nil {
			logFile.Close()
		}
		return "", nil, nil, fmt.Errorf("启动后台服务失败：%v（日志：%s）", err, logPath)
	}

	exited := make(chan error, 1)
	go func() { exited <- cmd.Wait() }()

	var stopOnce sync.Once
	stopFn := func() {
		stopOnce.Do(func() {
			if cmd.Process != nil {
				killProcessTree(cmd.Process.Pid)
			}
			if logFile != nil {
				logFile.Close()
			}
		})
	}

	wait = func(ctx context.Context) error {
		ctx, cancel := context.WithTimeout(ctx, 60*time.Second)
		defer cancel()
		ticker := time.NewTicker(50 * time.Millisecond)
		defer ticker.Stop()
		for {
			probeCtx, probeCancel := context.WithTimeout(ctx, 150*time.Millisecond)
			h, ok := healthAt(probeCtx, port)
			probeCancel()
			if ok && h.Name == "Velmira" {
				return nil
			}
			select {
			case <-ctx.Done():
				stopFn()
				if ctx.Err() == context.Canceled {
					return ctx.Err()
				}
				return fmt.Errorf("后台服务启动超时（60s），见日志：%s", logPath)
			case werr := <-exited:
				stopFn()
				return fmt.Errorf("后台服务提前退出（%v），见日志：%s", werr, logPath)
			case <-ticker.C:
			}
		}
	}
	return fmt.Sprintf("http://127.0.0.1:%d", port), stopFn, wait, nil
}

func backendCommand(root string, port int) (*exec.Cmd, error) {
	// 显式 Python 参数仍可用于开发；普通分发包优先使用内置边车。
	if *flagPython == "" && os.Getenv("VELMIRA_PYTHON") == "" {
		if info, err := os.Stat(sidecarPath(root)); err == nil && !info.IsDir() {
			return exec.Command(sidecarPath(root), "--host", *flagBind, "--port", strconv.Itoa(port)), nil
		}
	}
	py := *flagPython
	if py == "" {
		py = os.Getenv("VELMIRA_PYTHON")
	}
	if py == "" {
		py = "python"
	}
	if _, err := exec.LookPath(py); err != nil {
		if p, e := exec.LookPath("py"); e == nil {
			py = p
		} else {
			return nil, fmt.Errorf("找不到内置后端或 Python（尝试 %q）：请使用完整桌面分发包，或用 -python 指定", py)
		}
	}

	return exec.Command(py, "-m", "uvicorn", "server.app.main:app", "--host", *flagBind, "--port", strconv.Itoa(port), "--no-proxy-headers"), nil
}

// rotateBackendLog keeps the long-lived desktop log bounded. Rotation happens
// once per launch, before the new append handle is opened.
func rotateBackendLog(path string, limit int64, keep int) {
	info, err := os.Stat(path)
	if err != nil || info.Size() < limit {
		return
	}
	for index := keep - 1; index >= 1; index-- {
		_ = os.Rename(fmt.Sprintf("%s.%d", path, index), fmt.Sprintf("%s.%d", path, index+1))
	}
	_ = os.Rename(path, path+".1")
}

func backendLogPath() string {
	dir, err := os.UserCacheDir()
	if err != nil {
		dir = os.TempDir()
	}
	dir = filepath.Join(dir, "Velmira")
	_ = os.MkdirAll(dir, 0o755)
	return filepath.Join(dir, "backend.log")
}

// ---------- menus, fatal window ----------

func dataDir(root string) string {
	if v := os.Getenv("DATA_DIR"); v != "" {
		return v
	}
	if _, err := os.Stat(filepath.Join(root, "server", "app", "main.py")); err == nil {
		return filepath.Join(root, "data")
	}
	if _, err := os.Stat(sidecarPath(root)); err == nil {
		if cache, err := os.UserCacheDir(); err == nil {
			return filepath.Join(cache, "Velmira", "data")
		}
	}
	return filepath.Join(root, "data")
}

func showFatal(err error) {
	mygo.App.WhenReady(func() {
		win := mygo.NewWindow(mygo.WindowOptions{Title: "Velmira", Width: 720, Height: 420})
		win.Page().LoadHTML(fatalHTML(err), "")
	})
	if err := mygo.App.Run(); err != nil {
		log.Fatal(err)
	}
}

const startupHTML = `<!doctype html><html lang="zh-CN"><meta charset="utf-8"><style>
html,body{height:100%;margin:0;background:#f6f3ec;color:#38332b;font:15px system-ui,"Microsoft YaHei",sans-serif}
body{display:grid;place-content:center;text-align:center}h1{font-size:28px;letter-spacing:.04em;margin:0 0 14px}p{color:#796d5c}
@media(prefers-color-scheme:dark){html,body{background:#171614;color:#efe7d8}p{color:#b9ae9d}}
</style><title>Velmira</title><h1>Velmira</h1><p role="status">正在打开书架…</p></html>`

func fatalHTML(err error) string {
	return fmt.Sprintf(`<!doctype html><html><head><meta charset="utf-8"><style>
		html,body{height:100%%;margin:0;background:#171614;color:#efe7d8;font:15px/1.7 system-ui,"Microsoft YaHei",sans-serif}
		main{max-width:600px;margin:0 auto;padding:56px 32px}h1{font-size:26px;margin:0 0 6px}
		p{color:#b9ae9d}code{background:#26302b;padding:2px 6px;border-radius:4px;color:#d8c7a8}
		</style></head><body><main><h1>Velmira 无法启动</h1>
		<p>%s</p>
		<p>请确认桌面分发包已完整解压、内置 backend 文件夹存在，且后台端口可用。源码运行需要 Python 3.11+。启动日志在 <code>%%LOCALAPPDATA%%\Velmira\backend.log</code>。</p>
		</main></body></html>`, htmlEscape(err.Error()))
}

func htmlEscape(s string) string {
	replacer := strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", `"`, "&quot;")
	return replacer.Replace(s)
}
