package cmd

import (
	"fmt"

	"connectrpc.com/connect"
	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	v1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
	"github.com/spf13/cobra"
)

var logsLimit int32

var logsCmd = &cobra.Command{
	Use:   "logs <service-id>",
	Short: "Fetch build log lines for a service",
	Args:  cobra.ExactArgs(1),
	RunE:  runLogs,
}

func init() {
	logsCmd.Flags().Int32Var(&logsLimit, "limit", 100, "max number of log lines to return")
}

func runLogs(_ *cobra.Command, args []string) error {
	t := requireToken()
	serviceID := args[0]

	client := v1connect.NewServiceServiceClient(newHTTPClient(), apiURL(), connect.WithGRPC())
	req := withToken(connect.NewRequest(&v1.GetServiceLogsRequest{
		ServiceId: serviceID,
		Limit:     logsLimit,
	}), t)

	resp, err := client.GetServiceLogs(cmdCtx(), req)
	if err != nil {
		return fmt.Errorf("GetServiceLogs: %w", err)
	}

	lines := resp.Msg.GetLines()
	if len(lines) == 0 {
		fmt.Println("no log lines found")
		return nil
	}

	for _, l := range lines {
		if ts := l.GetTs(); ts != nil {
			fmt.Printf("[%s] %s\n", ts.AsTime().Format("15:04:05"), l.GetMessage())
		} else {
			fmt.Println(l.GetMessage())
		}
	}
	return nil
}
