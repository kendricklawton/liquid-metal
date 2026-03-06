package main

import (
	"os"

	"github.com/kendricklawton/liquid-metal/cli/cmd"
)

func main() {
	if err := cmd.Execute(); err != nil {
		os.Exit(1)
	}
}
