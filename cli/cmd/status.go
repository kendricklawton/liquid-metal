package cmd

import (
	"fmt"
	"os"
	"text/tabwriter"

	"connectrpc.com/connect"
	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	v1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
	"github.com/spf13/cobra"
)

var statusCmd = &cobra.Command{
	Use:   "status",
	Short: "List all services in your workspace",
	RunE:  runStatus,
}

func runStatus(_ *cobra.Command, _ []string) error {
	t := requireToken()

	client := v1connect.NewServiceServiceClient(newHTTPClient(), apiURL())
	req := withToken(connect.NewRequest(&v1.ListServicesRequest{}), t)

	resp, err := client.ListServices(cmdCtx(), req)
	if err != nil {
		return fmt.Errorf("ListServices: %w", err)
	}

	svcs := resp.Msg.GetServices()
	if len(svcs) == 0 {
		fmt.Println("no services found")
		return nil
	}

	w := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(w, "NAME\tENGINE\tSTATUS\tUPSTREAM")
	for _, s := range svcs {
		fmt.Fprintf(w, "%s\t%s\t%s\t%s\n",
			s.GetName(),
			engineLabel(s.GetEngine()),
			statusLabel(s.GetStatus()),
			s.GetUpstreamAddr(),
		)
	}
	w.Flush()
	return nil
}

func engineLabel(e v1.Engine) string {
	switch e {
	case v1.Engine_ENGINE_METAL:
		return "metal"
	case v1.Engine_ENGINE_LIQUID:
		return "liquid"
	default:
		return "unknown"
	}
}

func statusLabel(s v1.ServiceStatus) string {
	switch s {
	case v1.ServiceStatus_SERVICE_STATUS_RUNNING:
		return "running"
	case v1.ServiceStatus_SERVICE_STATUS_PROVISIONING:
		return "provisioning"
	case v1.ServiceStatus_SERVICE_STATUS_FAILED:
		return "failed"
	case v1.ServiceStatus_SERVICE_STATUS_STOPPED:
		return "stopped"
	default:
		return "unknown"
	}
}
