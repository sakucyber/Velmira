//go:build !windows

package main

import (
	"os"
	"os/exec"
)

func hideWindow(cmd *exec.Cmd) {}

func killProcessTree(pid int) {
	if p, err := os.FindProcess(pid); err == nil {
		_ = p.Kill()
	}
}
