package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"

	"connectrpc.com/connect"
	toml "github.com/pelletier/go-toml/v2"
	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	v1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
	"github.com/spf13/cobra"
	"github.com/spf13/viper"
)

var initCmd = &cobra.Command{
	Use:   "init",
	Short: "Initialize a new service in the current directory",
	Long: `Creates a project in your workspace and writes liquid-metal.toml.

Run this once per service directory before your first deploy.
Edit the generated liquid-metal.toml to change the engine, build command, or resource spec.`,
	RunE: runInit,
}

func runInit(_ *cobra.Command, _ []string) error {
	t := requireToken()

	workspaceID := viper.GetString("workspace_id")
	if workspaceID == "" {
		return fmt.Errorf("no workspace found — run `flux login` first")
	}

	// Refuse to overwrite an existing config.
	if _, err := os.Stat("liquid-metal.toml"); err == nil {
		return fmt.Errorf("liquid-metal.toml already exists — edit it directly or delete it to re-init")
	}

	cwd, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("get working directory: %w", err)
	}
	name := toSlug(filepath.Base(cwd))

	fmt.Printf("Initializing service %q in workspace %s...\n\n", name, workspaceID)

	// Create a project for this service.
	projectClient := v1connect.NewProjectServiceClient(newHTTPClient(), apiURL(), connect.WithGRPC())
	resp, err := projectClient.CreateProject(cmdCtx(), withToken(connect.NewRequest(&v1.CreateProjectRequest{
		WorkspaceId: workspaceID,
		Name:        name,
		Slug:        name,
	}), t))
	if err != nil {
		return fmt.Errorf("create project: %w", err)
	}
	projectID := resp.Msg.GetProject().GetId()

	// Write liquid-metal.toml.
	type serviceSection struct {
		Name      string `toml:"name"`
		Engine    string `toml:"engine"`
		ProjectID string `toml:"project_id"`
	}
	type buildSection struct {
		Command string `toml:"command"`
		Output  string `toml:"output"`
	}
	type liquidMetalConfig struct {
		Service serviceSection `toml:"service"`
		Build   buildSection   `toml:"build"`
	}

	f, err := os.Create("liquid-metal.toml")
	if err != nil {
		return fmt.Errorf("create liquid-metal.toml: %w", err)
	}
	defer f.Close()

	if err := toml.NewEncoder(f).Encode(liquidMetalConfig{
		Service: serviceSection{
			Name:      name,
			Engine:    "liquid",
			ProjectID: projectID,
		},
		Build: buildSection{
			Command: "GOOS=wasip1 GOARCH=wasm go build -o main.wasm .",
			Output:  "main.wasm",
		},
	}); err != nil {
		return fmt.Errorf("write liquid-metal.toml: %w", err)
	}

	fmt.Printf("Created liquid-metal.toml\n")
	fmt.Printf("  service: %s\n", name)
	fmt.Printf("  project: %s\n", projectID)
	fmt.Printf("  engine:  liquid\n\n")
	fmt.Printf("Edit liquid-metal.toml if needed, then run:\n\n  flux deploy\n")
	return nil
}

// toSlug converts a string into a URL-safe lowercase slug.
var nonAlphanumRe = regexp.MustCompile(`[^a-z0-9]+`)

func toSlug(s string) string {
	s = strings.ToLower(s)
	s = nonAlphanumRe.ReplaceAllString(s, "-")
	s = strings.Trim(s, "-")
	return s
}

// isConfigNotFound returns true when viper cannot locate the config file.
func isConfigNotFound(err error) bool {
	_, ok := err.(viper.ConfigFileNotFoundError)
	return ok
}
