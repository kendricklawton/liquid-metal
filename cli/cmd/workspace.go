package cmd

import (
	"fmt"
	"text/tabwriter"
	"os"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"github.com/spf13/viper"

	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	v1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
)

var workspaceCmd = &cobra.Command{
	Use:     "workspace",
	Short:   "Manage workspaces",
	Aliases: []string{"ws"},
}

var workspaceListCmd = &cobra.Command{
	Use:   "list",
	Short: "List your workspaces",
	RunE:  runWorkspaceList,
}

var workspaceUseCmd = &cobra.Command{
	Use:   "use <slug-or-id>",
	Short: "Switch active workspace",
	Args:  cobra.ExactArgs(1),
	RunE:  runWorkspaceUse,
}

func init() {
	workspaceCmd.AddCommand(workspaceListCmd, workspaceUseCmd)
}

func runWorkspaceList(_ *cobra.Command, _ []string) error {
	t := requireToken()
	active := viper.GetString("workspace_id")

	client := v1connect.NewWorkspaceServiceClient(newHTTPClient(), apiURL(), connect.WithGRPC())
	resp, err := client.ListWorkspaces(cmdCtx(), withToken(connect.NewRequest(&v1.ListWorkspacesRequest{}), t))
	if err != nil {
		return fmt.Errorf("list workspaces: %w", err)
	}

	workspaces := resp.Msg.GetWorkspaces()
	if len(workspaces) == 0 {
		fmt.Println("No workspaces found.")
		return nil
	}

	w := tabwriter.NewWriter(os.Stdout, 0, 0, 2, ' ', 0)
	fmt.Fprintln(w, "  SLUG\tNAME\tTIER\tID")
	for _, ws := range workspaces {
		marker := "  "
		if ws.GetId() == active {
			marker = "* "
		}
		fmt.Fprintf(w, "%s%s\t%s\t%s\t%s\n",
			marker,
			ws.GetSlug(),
			ws.GetName(),
			tierLabel(ws.GetTier()),
			ws.GetId(),
		)
	}
	w.Flush()
	return nil
}

func tierLabel(t v1.BillingTier) string {
	switch t {
	case v1.BillingTier_BILLING_TIER_HOBBY:
		return "hobby"
	case v1.BillingTier_BILLING_TIER_PRO:
		return "pro"
	case v1.BillingTier_BILLING_TIER_TEAM:
		return "team"
	default:
		return "unknown"
	}
}

func runWorkspaceUse(_ *cobra.Command, args []string) error {
	t := requireToken()
	target := args[0]

	client := v1connect.NewWorkspaceServiceClient(newHTTPClient(), apiURL(), connect.WithGRPC())
	resp, err := client.ListWorkspaces(cmdCtx(), withToken(connect.NewRequest(&v1.ListWorkspacesRequest{}), t))
	if err != nil {
		return fmt.Errorf("list workspaces: %w", err)
	}

	for _, ws := range resp.Msg.GetWorkspaces() {
		if ws.GetId() == target || ws.GetSlug() == target {
			viper.Set("workspace_id", ws.GetId())
			if err := viper.WriteConfig(); err != nil {
				return fmt.Errorf("save config: %w", err)
			}
			fmt.Printf("Switched to workspace: %s\n", ws.GetSlug())
			return nil
		}
	}

	return fmt.Errorf("workspace %q not found", target)
}
