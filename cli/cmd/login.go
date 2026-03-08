package cmd

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"

	"connectrpc.com/connect"
	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	v1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
	"github.com/spf13/cobra"
	"github.com/spf13/viper"
	"gopkg.in/yaml.v3"
)

const (
	workosAuthURL  = "https://api.workos.com/user_management/authorize"
	workosTokenURL = "https://api.workos.com/user_management/authenticate"

	// Default local callback port. Register http://localhost:8765/callback in
	// your WorkOS dashboard. Override with FLUX_CLI_PORT or --cli-port.
	defaultCLIPort = 8765
)

var loginCmd = &cobra.Command{
	Use:   "login",
	Short: "Authenticate with Liquid Metal via browser",
	RunE:  runLogin,
}

func runLogin(_ *cobra.Command, _ []string) error {
	clientID := viper.GetString("workos_client_id")
	if clientID == "" {
		return fmt.Errorf(
			"WorkOS client ID not configured\n\n" +
				"Set it once with:\n" +
				"  export WORKOS_CLIENT_ID=client_<your-id>\n\n" +
				"Find your client ID at: https://dashboard.workos.com/",
		)
	}

	port := viper.GetInt("cli_port")
	if port == 0 {
		port = defaultCLIPort
	}
	redirectURI := fmt.Sprintf("http://localhost:%d/callback", port)

	// ── PKCE ─────────────────────────────────────────────────────────────────
	verifier, err := newCodeVerifier()
	if err != nil {
		return fmt.Errorf("pkce: %w", err)
	}
	challenge := codeChallenge(verifier)

	state, err := newState()
	if err != nil {
		return fmt.Errorf("csrf state: %w", err)
	}

	// ── WorkOS authorization URL ──────────────────────────────────────────────
	q := url.Values{
		"response_type":         {"code"},
		"client_id":             {clientID},
		"redirect_uri":          {redirectURI},
		"code_challenge":        {challenge},
		"code_challenge_method": {"S256"},
		"provider":              {"authkit"},
		"state":                 {state},
	}
	authURL := workosAuthURL + "?" + q.Encode()

	// ── Local callback server ─────────────────────────────────────────────────
	codeCh := make(chan string, 1)
	errCh := make(chan error, 1)

	mux := http.NewServeMux()
	srv := &http.Server{Addr: fmt.Sprintf("127.0.0.1:%d", port), Handler: mux}

	mux.HandleFunc("/callback", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Query().Get("state") != state {
			http.Error(w, "invalid state", http.StatusBadRequest)
			errCh <- fmt.Errorf("state mismatch — possible CSRF")
			return
		}
		if e := r.URL.Query().Get("error"); e != "" {
			desc := r.URL.Query().Get("error_description")
			if desc == "" {
				desc = e
			}
			http.Error(w, desc, http.StatusBadRequest)
			errCh <- fmt.Errorf("authentication failed: %s", desc)
			return
		}
		code := r.URL.Query().Get("code")
		if code == "" {
			http.Error(w, "missing authorization code", http.StatusBadRequest)
			errCh <- fmt.Errorf("no code in callback")
			return
		}

		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, loginSuccessPage)

		codeCh <- code
		go srv.Shutdown(context.Background())
	})

	ln, err := net.Listen("tcp", fmt.Sprintf("127.0.0.1:%d", port))
	if err != nil {
		return fmt.Errorf(
			"could not bind port %d: %w\n\nOverride with: export FLUX_CLI_PORT=<port>",
			port, err,
		)
	}

	go func() {
		if serveErr := srv.Serve(ln); serveErr != nil && serveErr != http.ErrServerClosed {
			errCh <- serveErr
		}
	}()

	fmt.Printf("Opening browser to authenticate...\n\n  %s\n\nWaiting for login...\n", authURL)
	openBrowser(authURL)

	var code string
	select {
	case code = <-codeCh:
	case err = <-errCh:
		return err
	}

	// ── Exchange code + verifier → WorkOS access token ────────────────────────
	fmt.Print("Exchanging code... ")
	accessToken, err := exchangeCode(clientID, code, verifier, redirectURI)
	if err != nil {
		return fmt.Errorf("token exchange: %w", err)
	}
	fmt.Println("done.")

	// ── Provision user in Liquid Metal ────────────────────────────────────────
	fmt.Print("Provisioning account... ")
	userID, displayName, err := provisionViaCLI(accessToken)
	if err != nil {
		return fmt.Errorf("provision: %w", err)
	}
	fmt.Println("done.")

	if err := saveConfig(userID, clientID, port); err != nil {
		return fmt.Errorf("save config: %w", err)
	}

	fmt.Printf("\nWelcome back, %s!\n", displayName)
	fmt.Printf("Config saved to ~/.config/flux/config.yaml\n")
	return nil
}

// ── PKCE ─────────────────────────────────────────────────────────────────────

func newCodeVerifier() (string, error) {
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(b), nil
}

func codeChallenge(verifier string) string {
	h := sha256.Sum256([]byte(verifier))
	return base64.RawURLEncoding.EncodeToString(h[:])
}

func newState() (string, error) {
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(b), nil
}

// ── WorkOS token exchange ─────────────────────────────────────────────────────

func exchangeCode(clientID, code, verifier, redirectURI string) (string, error) {
	body, _ := json.Marshal(map[string]string{
		"grant_type":    "authorization_code",
		"client_id":     clientID,
		"code":          code,
		"code_verifier": verifier,
		"redirect_uri":  redirectURI,
	})

	resp, err := http.Post(workosTokenURL, "application/json", bytes.NewReader(body))
	if err != nil {
		return "", fmt.Errorf("POST to WorkOS: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("WorkOS returned %d", resp.StatusCode)
	}

	var tr struct {
		AccessToken string `json:"access_token"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&tr); err != nil {
		return "", fmt.Errorf("decode: %w", err)
	}
	if tr.AccessToken == "" {
		return "", fmt.Errorf("empty access_token in WorkOS response")
	}
	return tr.AccessToken, nil
}

// ── Liquid Metal provisioning ─────────────────────────────────────────────────

func provisionViaCLI(accessToken string) (string, string, error) {
	body, _ := json.Marshal(map[string]string{"access_token": accessToken})

	resp, err := http.Post(apiURL()+"/auth/cli/provision", "application/json", bytes.NewReader(body))
	if err != nil {
		return "", "", fmt.Errorf("POST to API: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return "", "", fmt.Errorf("API returned %d", resp.StatusCode)
	}

	var pr struct {
		ID   string `json:"id"`
		Name string `json:"name"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&pr); err != nil {
		return "", "", fmt.Errorf("decode provision response: %w", err)
	}
	if pr.ID == "" {
		return "", "", fmt.Errorf("no user ID in provision response")
	}
	if pr.Name == "" {
		pr.Name = pr.ID
	}
	return pr.ID, pr.Name, nil
}

// ── Config ────────────────────────────────────────────────────────────────────

func saveConfig(token, workosClientID string, cliPort int) error {
	home, _ := os.UserHomeDir()
	dir := filepath.Join(home, ".config", "flux")
	if err := os.MkdirAll(dir, 0700); err != nil {
		return err
	}

	apiVal := viper.GetString("api_url")
	if apiVal == "" {
		apiVal = "http://localhost:7070"
	}

	cfg := map[string]any{
		"token":            token,
		"api_url":          apiVal,
		"workos_client_id": workosClientID,
	}
	if cliPort != defaultCLIPort {
		cfg["cli_port"] = cliPort
	}

	f, err := os.OpenFile(filepath.Join(dir, "config.yaml"), os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0600)
	if err != nil {
		return err
	}
	defer f.Close()
	return yaml.NewEncoder(f).Encode(cfg)
}

// printWelcome is available for future use by other commands.
func printWelcome(token string) {
	client := v1connect.NewUserServiceClient(newHTTPClient(), apiURL())
	req := withToken(connect.NewRequest(&v1.GetMeRequest{}), token)
	resp, err := client.GetMe(context.Background(), req)
	if err != nil {
		fmt.Println("Logged in. Run `flux whoami` to verify.")
		return
	}
	u := resp.Msg.GetUser()
	name := u.GetName()
	if name == "" {
		name = u.GetEmail()
	}
	fmt.Printf("\nWelcome back, %s!\n", name)
}

// ── Browser ───────────────────────────────────────────────────────────────────

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

const loginSuccessPage = `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Liquid Metal</title>
  <style>
    *{box-sizing:border-box;margin:0;padding:0}
    body{font-family:system-ui,-apple-system,sans-serif;background:#0a0a0a;color:#fff;
         display:flex;align-items:center;justify-content:center;height:100vh}
    h1{font-size:1.4rem;font-weight:500;margin-bottom:.5rem}
    p{font-size:.9rem;color:#666}
  </style>
</head>
<body>
  <div style="text-align:center">
    <h1>Login successful</h1>
    <p>You can close this tab and return to your terminal.</p>
  </div>
  <script>window.close()</script>
</body>
</html>`
