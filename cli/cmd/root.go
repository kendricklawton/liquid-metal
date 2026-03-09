package cmd

import (
	"context"
	"crypto/tls"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"github.com/spf13/viper"
	"golang.org/x/net/http2"
)

var cfgFile string

var rootCmd = &cobra.Command{
	Use:   "flux",
	Short: "flux — liquid-metal CLI",
}

// Execute is the entry point called by main.
func Execute() error {
	return rootCmd.Execute()
}

func init() {
	cobra.OnInitialize(initConfig)

	rootCmd.PersistentFlags().StringVar(&cfgFile, "config", "", "config file (default: ~/.config/flux/config.yaml)")
	rootCmd.PersistentFlags().String("api-url", "", "Rust API URL (overrides config)")
	rootCmd.PersistentFlags().String("token", "", "API token (overrides config)")
	rootCmd.PersistentFlags().String("client-id", "", "WorkOS client ID (overrides config, env: FLUX_WORKOS_CLIENT_ID)")
	rootCmd.PersistentFlags().Int("cli-port", 0, "local OAuth callback port (default 8765, env: FLUX_CLI_PORT)")

	viper.BindPFlag("api_url",          rootCmd.PersistentFlags().Lookup("api-url"))
	viper.BindPFlag("token",            rootCmd.PersistentFlags().Lookup("token"))
	viper.BindPFlag("workos_client_id", rootCmd.PersistentFlags().Lookup("client-id"))
	viper.BindPFlag("cli_port",         rootCmd.PersistentFlags().Lookup("cli-port"))

	// Hide internal/dev override flags from end-user help output.
	// They still work when passed explicitly — just not shown by default.
	rootCmd.PersistentFlags().MarkHidden("client-id")
	rootCmd.PersistentFlags().MarkHidden("cli-port")
	rootCmd.PersistentFlags().MarkHidden("api-url")

	rootCmd.AddCommand(loginCmd, logoutCmd, whoamiCmd, statusCmd, logsCmd, deployCmd)
}

func initConfig() {
	if cfgFile != "" {
		viper.SetConfigFile(cfgFile)
	} else {
		home, _ := os.UserHomeDir()
		viper.SetConfigFile(filepath.Join(home, ".config", "flux", "config.yaml"))
	}
	viper.SetEnvPrefix("FLUX")
	viper.AutomaticEnv()
	_ = viper.ReadInConfig()
}

func cmdCtx() context.Context { return context.Background() }

func apiURL() string {
	if u := viper.GetString("api_url"); u != "" {
		return u
	}
	return "http://localhost:7070"
}

func webURL() string {
	if u := viper.GetString("web_url"); u != "" {
		return u
	}
	return "http://localhost:3000"
}

func requireToken() string {
	t := viper.GetString("token")
	if t == "" {
		fmt.Fprintln(os.Stderr, "error: not logged in. Run: flux login")
		os.Exit(1)
	}
	return t
}

func newHTTPClient() *http.Client {
	return &http.Client{
		Transport: &http2.Transport{
			AllowHTTP: true,
			DialTLSContext: func(ctx context.Context, network, addr string, _ *tls.Config) (net.Conn, error) {
				return (&net.Dialer{}).DialContext(ctx, network, addr)
			},
		},
	}
}

func withToken[T any](req *connect.Request[T], t string) *connect.Request[T] {
	req.Header().Set("X-Api-Key", t)
	return req
}
