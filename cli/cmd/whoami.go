package cmd

import (
	"fmt"

	"connectrpc.com/connect"
	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	v1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
	"github.com/spf13/cobra"
	"github.com/spf13/viper"
)

var whoamiCmd = &cobra.Command{
	Use:   "whoami",
	Short: "Show the currently authenticated user",
	RunE:  runWhoami,
}

func runWhoami(_ *cobra.Command, _ []string) error {
	t := requireToken()
	activeWS := viper.GetString("workspace_id")

	userClient := v1connect.NewUserServiceClient(newHTTPClient(), apiURL(), connect.WithGRPC())
	meResp, err := userClient.GetMe(cmdCtx(), withToken(connect.NewRequest(&v1.GetMeRequest{}), t))
	if err != nil {
		return fmt.Errorf("GetMe: %w", err)
	}

	u := meResp.Msg.GetUser()
	fmt.Printf("name:  %s\n", u.GetName())
	fmt.Printf("email: %s\n", u.GetEmail())
	fmt.Printf("id:    %s\n", u.GetId())

	wsClient := v1connect.NewWorkspaceServiceClient(newHTTPClient(), apiURL(), connect.WithGRPC())
	wsResp, err := wsClient.ListWorkspaces(cmdCtx(), withToken(connect.NewRequest(&v1.ListWorkspacesRequest{}), t))
	if err != nil {
		return nil // not fatal — user info already printed
	}

	for _, ws := range wsResp.Msg.GetWorkspaces() {
		marker := "  "
		if ws.GetId() == activeWS {
			marker = "* "
		}
		fmt.Printf("%sworkspace: %s  tier: %s\n", marker, ws.GetSlug(), tierLabel(ws.GetTier()))
	}
	return nil
}
