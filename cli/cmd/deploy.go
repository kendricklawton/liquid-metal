package cmd

import (
	"fmt"
	"os"
	"strings"

	"connectrpc.com/connect"
	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	v1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
	"github.com/spf13/cobra"
	"github.com/spf13/viper"
)

var deployCmd = &cobra.Command{
	Use:   "deploy",
	Short: "Deploy a service from machine.toml",
	RunE:  runDeploy,
}

func runDeploy(_ *cobra.Command, _ []string) error {
	t := requireToken()

	cfg := viper.New()
	cfg.SetConfigName("machine")
	cfg.SetConfigType("toml")
	cfg.AddConfigPath(".")
	if err := cfg.ReadInConfig(); err != nil {
		return fmt.Errorf("read machine.toml: %w (run from your project directory)", err)
	}

	name := cfg.GetString("service.name")
	engineStr := strings.ToLower(cfg.GetString("service.engine"))
	port := cfg.GetInt32("service.port")
	vcpu := cfg.GetInt32("metal.vcpu")
	memoryMB := cfg.GetInt32("metal.memory_mb")

	if name == "" {
		return fmt.Errorf("machine.toml: [service].name is required")
	}

	var engine v1.Engine
	switch engineStr {
	case "metal":
		engine = v1.Engine_ENGINE_METAL
	case "liquid":
		engine = v1.Engine_ENGINE_LIQUID
	default:
		return fmt.Errorf("machine.toml: [service].engine must be 'metal' or 'liquid', got %q", engineStr)
	}

	fmt.Printf("deploying %s (engine: %s)...\n", name, engineStr)

	client := v1connect.NewServiceServiceClient(newHTTPClient(), apiURL())
	req := withToken(connect.NewRequest(&v1.CreateServiceRequest{
		Name:     name,
		Engine:   engine,
		Port:     port,
		Vcpu:     vcpu,
		MemoryMb: memoryMB,
	}), t)

	resp, err := client.CreateService(cmdCtx(), req)
	if err != nil {
		return fmt.Errorf("CreateService: %w", err)
	}

	svc := resp.Msg.GetService()
	fmt.Fprintf(os.Stdout, "service created\n")
	fmt.Fprintf(os.Stdout, "  id:     %s\n", svc.GetId())
	fmt.Fprintf(os.Stdout, "  slug:   %s\n", svc.GetSlug())
	fmt.Fprintf(os.Stdout, "  status: %s\n", statusLabel(svc.GetStatus()))
	return nil
}
