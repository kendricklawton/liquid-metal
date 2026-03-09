package cmd

import (
	"fmt"

	"connectrpc.com/connect"
	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	v1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
	"github.com/spf13/cobra"
)

var restartCmd = &cobra.Command{
	Use:   "restart <service-id>",
	Short: "Restart a stopped or failed service",
	Args:  cobra.ExactArgs(1),
	RunE:  runRestart,
}

func runRestart(_ *cobra.Command, args []string) error {
	t := requireToken()

	client := v1connect.NewServiceServiceClient(newHTTPClient(), apiURL(), connect.WithGRPC())
	resp, err := client.RestartService(cmdCtx(), withToken(connect.NewRequest(&v1.RestartServiceRequest{
		Id: args[0],
	}), t))
	if err != nil {
		return fmt.Errorf("RestartService: %w", err)
	}

	svc := resp.Msg.GetService()
	fmt.Printf("Restarting %s — status: %s\n", svc.GetSlug(), statusLabel(svc.GetStatus()))
	return nil
}
