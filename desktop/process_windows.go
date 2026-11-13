//go:build windows

package main

import (
	"os/exec"
	"strconv"
	"syscall"
)

// hideWindow keeps the backend console window from flashing on screen.
func hideWindow(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{HideWindow: true, CreationFlags: 0x08000000}
}

// killProcessTree stops the backend and anything it spawned (uvicorn workers).
func killProcessTree(pid int) {
	cmd := exec.Command("taskkill", "/PID", strconv.Itoa(pid), "/T", "/F")
	// taskkill is a console program; keep its cleanup window hidden when the app closes.
	hideWindow(cmd)
	_ = cmd.Run()
}
