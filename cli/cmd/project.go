package cmd

import (
	"fmt"
	"os"
	"text/tabwriter"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"github.com/spf13/viper"

	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	v1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
)

var projectCmd = &cobra.Command{
	Use:   "project",
	Short: "Manage projects",
}

var projectListCmd = &cobra.Command{
	Use:   "list",
	Short: "List projects in the active workspace",
	RunE:  runProjectList,
}

var projectUseCmd = &cobra.Command{
	Use:   "use <slug-or-id>",
	Short: "Set project_id in ./liquid-metal.toml",
	Args:  cobra.ExactArgs(1),
	RunE:  runProjectUse,
}

func init() {
	projectCmd.AddCommand(projectListCmd, projectUseCmd)
}

func runProjectList(_ *cobra.Command, _ []string) error {
	t := requireToken()
	workspaceID := viper.GetString("workspace_id")
	if workspaceID == "" {
		return fmt.Errorf("no active workspace — run `flux workspace use <slug>` first")
	}

	// Show the project_id currently in liquid-metal.toml (if any) as the active marker.
	activeCfg := viper.New()
	activeCfg.SetConfigName("liquid-metal")
	activeCfg.SetConfigType("toml")
	activeCfg.AddConfigPath(".")
	_ = activeCfg.ReadInConfig()
	activeProject := activeCfg.GetString("service.project_id")

	client := v1connect.NewProjectServiceClient(newHTTPClient(), apiURL(), connect.WithGRPC())
	resp, err := client.ListProjects(cmdCtx(), withToken(connect.NewRequest(&v1.ListProjectsRequest{
		WorkspaceId: workspaceID,
	}), t))
	if err != nil {
		return fmt.Errorf("list projects: %w", err)
	}

	projects := resp.Msg.GetProjects()
	if len(projects) == 0 {
		fmt.Println("No projects found in this workspace.")
		return nil
	}

	w := tabwriter.NewWriter(os.Stdout, 0, 0, 2, ' ', 0)
	fmt.Fprintln(w, "  SLUG\tNAME\tID")
	for _, p := range projects {
		marker := "  "
		if p.GetId() == activeProject {
			marker = "* "
		}
		fmt.Fprintf(w, "%s%s\t%s\t%s\n",
			marker,
			p.GetSlug(),
			p.GetName(),
			p.GetId(),
		)
	}
	w.Flush()
	return nil
}

func runProjectUse(_ *cobra.Command, args []string) error {
	t := requireToken()
	target := args[0]
	workspaceID := viper.GetString("workspace_id")
	if workspaceID == "" {
		return fmt.Errorf("no active workspace — run `flux workspace use <slug>` first")
	}

	client := v1connect.NewProjectServiceClient(newHTTPClient(), apiURL(), connect.WithGRPC())
	resp, err := client.ListProjects(cmdCtx(), withToken(connect.NewRequest(&v1.ListProjectsRequest{
		WorkspaceId: workspaceID,
	}), t))
	if err != nil {
		return fmt.Errorf("list projects: %w", err)
	}

	var projectID string
	for _, p := range resp.Msg.GetProjects() {
		if p.GetId() == target || p.GetSlug() == target {
			projectID = p.GetId()
			break
		}
	}
	if projectID == "" {
		return fmt.Errorf("project %q not found in active workspace", target)
	}

	// Update (or create) ./liquid-metal.toml with the new project_id.
	if err := setProjectInToml(projectID); err != nil {
		return fmt.Errorf("update liquid-metal.toml: %w", err)
	}

	fmt.Printf("Set project_id in liquid-metal.toml: %s\n", projectID)
	return nil
}

// setProjectInToml writes project_id into ./liquid-metal.toml, preserving other keys.
func setProjectInToml(projectID string) error {
	cfg := viper.New()
	cfg.SetConfigName("liquid-metal")
	cfg.SetConfigType("toml")
	cfg.AddConfigPath(".")
	_ = cfg.ReadInConfig() // OK if file doesn't exist yet

	cfg.Set("service.project_id", projectID)

	// If no config file was loaded, write a new one.
	if cfg.ConfigFileUsed() == "" {
		cfg.SetConfigFile("liquid-metal.toml")
	}
	return cfg.WriteConfig()
}
