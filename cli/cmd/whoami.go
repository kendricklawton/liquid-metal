package cmd

import (
	"fmt"

	"connectrpc.com/connect"
	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	v1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
	"github.com/spf13/cobra"
)

var whoamiCmd = &cobra.Command{
	Use:   "whoami",
	Short: "Show the currently authenticated user",
	RunE:  runWhoami,
}

func runWhoami(_ *cobra.Command, _ []string) error {
	t := requireToken()

	client := v1connect.NewUserServiceClient(newHTTPClient(), apiURL())
	req := withToken(connect.NewRequest(&v1.GetMeRequest{}), t)

	resp, err := client.GetMe(cmdCtx(), req)
	if err != nil {
		return fmt.Errorf("GetMe: %w", err)
	}

	u := resp.Msg.GetUser()
	fmt.Printf("name:  %s\n", u.GetName())
	fmt.Printf("email: %s\n", u.GetEmail())
	fmt.Printf("id:    %s\n", u.GetId())
	return nil
}
