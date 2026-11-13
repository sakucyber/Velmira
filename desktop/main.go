// Velmira desktop — a MyGo app that launches the local Velmira server
// (Python + FastAPI) and shows its web UI in a native window.
//
//	cd desktop && go build -trimpath -ldflags '-H=windowsgui -s -w' -o Velmira.exe .
//	./Velmira.exe                      # normal desktop app
//	./Velmira.exe -screenshot shot.png # capture the window and quit (CI / evidence)
package main

import (
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
	baseURL, stop, err := ensureBackend(root, backendPort())
	if err != nil {
		showFatal(err)
		return
	}
	mygo.App.OnQuit(stop)
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
		if err := win.Page().LoadURL(baseURL + "/"); err != nil {
			log.Printf("load %s: %v", baseURL, err)
		}

		if *flagScreenshot != "" {
			go func() {
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
			}()
		}
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
	if _, err := os.Stat(oneFile); err == nil {
		return oneFile
	}
	// Directory-mode PyInstaller builds avoid extracting the backend on every launch.
	return filepath.Join(root, "backend", name, fileName)
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

func healthAt(port int) (health, bool) {
	client := &http.Client{Timeout: 1500 * time.Millisecond}
	resp, err := client.Get(fmt.Sprintf("http://127.0.0.1:%d/api/health", port))
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

func ensureBackend(root string, port int) (baseURL string, stop func(), err error) {
	// Reuse a server that is already running (dev convenience).
	if h, ok := healthAt(port); ok && h.Name == "Velmira" {
		return fmt.Sprintf("http://127.0.0.1:%d", port), func() {}, nil
	}
	// Pick a free port when the preferred one is taken by something else.
	listener, listenErr := net.Listen("tcp", net.JoinHostPort(*flagBind, strconv.Itoa(port)))
	if listenErr != nil {
		port = freePort()
	} else {
		listener.Close()
	}

	cmd, err := backendCommand(root, port)
	if err != nil {
		return "", nil, err
	}

	logPath := backendLogPath()
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
		return "", nil, fmt.Errorf("启动后台服务失败：%v（日志：%s）", err, logPath)
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

	deadline := time.Now().Add(60 * time.Second)
	for time.Now().Before(deadline) {
		if h, ok := healthAt(port); ok && h.OK && h.Name == "Velmira" {
			return fmt.Sprintf("http://127.0.0.1:%d", port), stopFn, nil
		}
		select {
		case werr := <-exited:
			stopFn()
			return "", nil, fmt.Errorf("后台服务提前退出（%v），见日志：%s", werr, logPath)
		default:
		}
		time.Sleep(250 * time.Millisecond)
	}
	stopFn()
	return "", nil, fmt.Errorf("后台服务启动超时（60s），见日志：%s", logPath)
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

	return exec.Command(py, "-m", "uvicorn", "server.app.main:app", "--host", *flagBind, "--port", strconv.Itoa(port)), nil
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
		html := fmt.Sprintf(`<!doctype html><html><head><meta charset="utf-8"><style>
		html,body{height:100%%;margin:0;background:#171614;color:#efe7d8;font:15px/1.7 system-ui,"Microsoft YaHei",sans-serif}
		main{max-width:600px;margin:0 auto;padding:56px 32px}h1{font-size:26px;margin:0 0 6px}
		p{color:#b9ae9d}code{background:#26302b;padding:2px 6px;border-radius:4px;color:#d8c7a8}
		</style></head><body><main><h1>Velmira 无法启动</h1>
		<p>%s</p>
		<p>请确认桌面分发包已完整解压、内置 backend 文件夹存在，且后台端口可用。源码运行需要 Python 3.11+。启动日志在 <code>%%LOCALAPPDATA%%\Velmira\backend.log</code>。</p>
		</main></body></html>`, htmlEscape(err.Error()))
		win.Page().LoadHTML(html, "")
	})
	if err := mygo.App.Run(); err != nil {
		log.Fatal(err)
	}
}

func htmlEscape(s string) string {
	replacer := strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", `"`, "&quot;")
	return replacer.Replace(s)
}
