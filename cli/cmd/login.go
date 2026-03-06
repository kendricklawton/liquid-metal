package cmd

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"

	"github.com/spf13/cobra"
	"github.com/spf13/viper"
	"gopkg.in/yaml.v3"
)

var loginCmd = &cobra.Command{
	Use:   "login",
	Short: "Authenticate with liquid-metal via browser",
	RunE:  runLogin,
}

func runLogin(_ *cobra.Command, _ []string) error {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return fmt.Errorf("find free port: %w", err)
	}
	port := ln.Addr().(*net.TCPAddr).Port
	ln.Close()

	redirectURI := fmt.Sprintf("http://localhost:%d/callback", port)
	loginURL := fmt.Sprintf("%s/auth/cli/login?redirect_uri=%s",
		webURL(), url.QueryEscape(redirectURI))

	tokenCh := make(chan string, 1)
	errCh := make(chan error, 1)

	mux := http.NewServeMux()
	srv := &http.Server{Addr: fmt.Sprintf("127.0.0.1:%d", port), Handler: mux}

	mux.HandleFunc("/callback", func(w http.ResponseWriter, r *http.Request) {
		t := r.URL.Query().Get("token")
		if t == "" {
			http.Error(w, "missing token", http.StatusBadRequest)
			errCh <- fmt.Errorf("callback received no token")
			return
		}
		fmt.Fprintln(w, "<html><body><p>Login successful — you can close this tab.</p><script>window.close()</script></body></html>")
		tokenCh <- t
		go srv.Shutdown(context.Background())
	})

	go func() {
		if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			errCh <- err
		}
	}()

	fmt.Printf("Opening browser...\n%s\n\nWaiting for login...\n", loginURL)
	openBrowser(loginURL)

	select {
	case t := <-tokenCh:
		if err := saveToken(t); err != nil {
			return fmt.Errorf("save config: %w", err)
		}
		fmt.Println("Logged in. Token saved to ~/.config/flux/config.yaml")
		return nil
	case err := <-errCh:
		return err
	}
}

func saveToken(t string) error {
	home, _ := os.UserHomeDir()
	dir := filepath.Join(home, ".config", "flux")
	if err := os.MkdirAll(dir, 0700); err != nil {
		return err
	}

	cfg := map[string]string{
		"token":   t,
		"api_url": viper.GetString("api_url"),
		"web_url": viper.GetString("web_url"),
	}
	if cfg["api_url"] == "" {
		cfg["api_url"] = "http://localhost:7070"
	}
	if cfg["web_url"] == "" {
		cfg["web_url"] = "http://localhost:3000"
	}

	f, err := os.OpenFile(filepath.Join(dir, "config.yaml"), os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0600)
	if err != nil {
		return err
	}
	defer f.Close()
	return yaml.NewEncoder(f).Encode(cfg)
}

func openBrowser(u string) {
	switch runtime.GOOS {
	case "darwin":
		exec.Command("open", u).Start()
	case "linux":
		exec.Command("xdg-open", u).Start()
	case "windows":
		exec.Command("cmd", "/c", "start", u).Start()
	}
}
