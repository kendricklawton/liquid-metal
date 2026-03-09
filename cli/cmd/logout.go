package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
)

var logoutCmd = &cobra.Command{
	Use:   "logout",
	Short: "Log out and remove saved credentials",
	RunE:  runLogout,
}

func runLogout(_ *cobra.Command, _ []string) error {
	home, _ := os.UserHomeDir()
	cfgPath := filepath.Join(home, ".config", "flux", "config.yaml")

	if err := os.Remove(cfgPath); err != nil {
		if os.IsNotExist(err) {
			fmt.Println("Not logged in.")
			return nil
		}
		return fmt.Errorf("removing config: %w", err)
	}

	fmt.Println("Logged out.")
	return nil
}
