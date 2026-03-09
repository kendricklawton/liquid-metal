package cmd

import (
	"fmt"

	"connectrpc.com/connect"
	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	v1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
	"github.com/spf13/cobra"
)

var stopCmd = &cobra.Command{
	Use:   "stop <service-id>",
	Short: "Stop a running service",
	Args:  cobra.ExactArgs(1),
	RunE:  runStop,
}

func runStop(_ *cobra.Command, args []string) error {
	t := requireToken()

	client := v1connect.NewServiceServiceClient(newHTTPClient(), apiURL(), connect.WithGRPC())
	_, err := client.StopService(cmdCtx(), withToken(connect.NewRequest(&v1.StopServiceRequest{
		Id: args[0],
	}), t))
	if err != nil {
		return fmt.Errorf("StopService: %w", err)
	}

	fmt.Printf("Service %s stopped.\n", args[0])
	return nil
}
