//go:build !windows

package main

import "os/exec"

// Allows protocol tests to run without a Windows desktop.
func configureCommand(_ *exec.Cmd) {}
