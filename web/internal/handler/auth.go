package handler

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"strings"
	"time"

	"github.com/workos/workos-go/v6/pkg/usermanagement"
)

const stateCookieName = "auth_state"
const cliRedirectCookieName = "auth_cli_redirect"

// AuthLogin generates a CSRF state and redirects to WorkOS AuthKit.
func (h *Handler) AuthLogin(w http.ResponseWriter, r *http.Request) {
	state, err := generateState()
	if err != nil {
		http.Error(w, "Failed to initialize login", http.StatusInternalServerError)
		return
	}

	http.SetCookie(w, &http.Cookie{
		Name:     stateCookieName,
		Value:    state,
		Path:     "/",
		HttpOnly: true,
		SameSite: http.SameSiteLaxMode,
		MaxAge:   300,
	})

	usermanagement.SetAPIKey(h.WorkOSAPIKey)
	authURL, err := usermanagement.GetAuthorizationURL(usermanagement.GetAuthorizationURLOpts{
		ClientID:    h.WorkOSClientID,
		RedirectURI: h.WorkOSRedirectURI,
		Provider:    "authkit",
		State:       state,
	})
	if err != nil {
		http.Error(w, "Failed to generate auth URL", http.StatusInternalServerError)
		return
	}

	http.Redirect(w, r, authURL.String(), http.StatusFound)
}

// AuthCallback handles the WorkOS OAuth redirect.
func (h *Handler) AuthCallback(w http.ResponseWriter, r *http.Request) {
	code := r.URL.Query().Get("code")
	returnedState := r.URL.Query().Get("state")

	stateCookie, err := r.Cookie(stateCookieName)
	if err != nil || returnedState == "" || stateCookie.Value != returnedState {
		http.Error(w, "Invalid or expired authentication state", http.StatusUnauthorized)
		return
	}
	http.SetCookie(w, &http.Cookie{Name: stateCookieName, Value: "", Path: "/", MaxAge: -1})

	usermanagement.SetAPIKey(h.WorkOSAPIKey)
	authResp, err := usermanagement.AuthenticateWithCode(r.Context(), usermanagement.AuthenticateWithCodeOpts{
		ClientID: h.WorkOSClientID,
		Code:     code,
	})
	if err != nil {
		log.Printf("WorkOS AuthenticateWithCode error: %v", err)
		http.Error(w, "Authentication failed", http.StatusInternalServerError)
		return
	}

	userID, userName, workspaceSlug, tier, err := h.provisionUserViaAPI(r.Context(), authResp.User.Email, authResp.User.FirstName, authResp.User.LastName)
	if err != nil {
		log.Printf("provisionUserViaAPI error: %v", err)
		http.Error(w, "Failed to provision user account", http.StatusInternalServerError)
		return
	}

	exp := time.Now().Add(7 * 24 * time.Hour)

	for _, c := range []http.Cookie{
		{Name: SessionCookieName, Value: userID},
		{Name: nameCookieName, Value: userName},
		{Name: emailCookieName, Value: authResp.User.Email},
		{Name: workosUIDCookieName, Value: authResp.User.ID},
		{Name: slugCookieName, Value: workspaceSlug},
		{Name: tierCookieName, Value: tier},
	} {
		c := c
		c.Path = "/"
		c.HttpOnly = true
		c.SameSite = http.SameSiteLaxMode
		c.Expires = exp
		http.SetCookie(w, &c)
	}

	if sid, err := extractSIDFromJWT(authResp.AccessToken); err == nil && sid != "" {
		http.SetCookie(w, &http.Cookie{
			Name:     workosSIDCookieName,
			Value:    sid,
			Path:     "/",
			HttpOnly: true,
			SameSite: http.SameSiteLaxMode,
			Expires:  exp,
		})
	}

	if workspaceSlug == "" {
		http.Redirect(w, r, "/dashboard", http.StatusFound)
		return
	}
	http.Redirect(w, r, "/"+workspaceSlug, http.StatusFound)
}

// AuthLogout clears all session cookies and revokes the WorkOS SSO session.
func (h *Handler) AuthLogout(w http.ResponseWriter, r *http.Request) {
	past := time.Unix(0, 0)
	for _, name := range []string{
		SessionCookieName, nameCookieName, emailCookieName,
		workosUIDCookieName, slugCookieName, workosSIDCookieName,
		stateCookieName, tierCookieName,
	} {
		http.SetCookie(w, &http.Cookie{
			Name:     name,
			Value:    "",
			Path:     "/",
			HttpOnly: true,
			SameSite: http.SameSiteLaxMode,
			MaxAge:   -1,
			Expires:  past,
		})
	}

	sidCookie, err := r.Cookie(workosSIDCookieName)
	if err == nil && sidCookie.Value != "" {
		usermanagement.SetAPIKey(h.WorkOSAPIKey)
		logoutURL, err := usermanagement.GetLogoutURL(usermanagement.GetLogoutURLOpts{
			SessionID: sidCookie.Value,
			ReturnTo:  h.BaseURL,
		})
		if err == nil {
			http.Redirect(w, r, logoutURL.String(), http.StatusSeeOther)
			return
		}
		log.Printf("AuthLogout: failed to build WorkOS logout URL: %v", err)
	}

	http.Redirect(w, r, h.BaseURL, http.StatusSeeOther)
}

// AuthCLILogin starts the browser OAuth flow for the flux CLI.
func (h *Handler) AuthCLILogin(w http.ResponseWriter, r *http.Request) {
	cliRedirectURI := r.URL.Query().Get("redirect_uri")
	if cliRedirectURI == "" || !strings.HasPrefix(cliRedirectURI, "http://localhost") {
		http.Error(w, "invalid redirect_uri: must be http://localhost", http.StatusBadRequest)
		return
	}

	state, err := generateState()
	if err != nil {
		http.Error(w, "failed to initialize login", http.StatusInternalServerError)
		return
	}

	http.SetCookie(w, &http.Cookie{Name: stateCookieName, Value: state, Path: "/", HttpOnly: true, SameSite: http.SameSiteLaxMode, MaxAge: 300})
	http.SetCookie(w, &http.Cookie{Name: cliRedirectCookieName, Value: cliRedirectURI, Path: "/", HttpOnly: true, SameSite: http.SameSiteLaxMode, MaxAge: 300})

	usermanagement.SetAPIKey(h.WorkOSAPIKey)
	authURL, err := usermanagement.GetAuthorizationURL(usermanagement.GetAuthorizationURLOpts{
		ClientID:    h.WorkOSClientID,
		RedirectURI: h.WorkOSCLIRedirectURI,
		Provider:    "authkit",
		State:       state,
	})
	if err != nil {
		http.Error(w, "failed to generate auth URL", http.StatusInternalServerError)
		return
	}

	http.Redirect(w, r, authURL.String(), http.StatusFound)
}

// AuthCLICallback exchanges the code and redirects to the CLI's local server with the token.
func (h *Handler) AuthCLICallback(w http.ResponseWriter, r *http.Request) {
	code := r.URL.Query().Get("code")
	returnedState := r.URL.Query().Get("state")

	stateCookie, err := r.Cookie(stateCookieName)
	if err != nil || returnedState == "" || stateCookie.Value != returnedState {
		http.Error(w, "invalid or expired authentication state", http.StatusUnauthorized)
		return
	}
	http.SetCookie(w, &http.Cookie{Name: stateCookieName, Value: "", Path: "/", MaxAge: -1})

	cliRedirectCookie, err := r.Cookie(cliRedirectCookieName)
	if err != nil || cliRedirectCookie.Value == "" {
		http.Error(w, "missing CLI redirect URI", http.StatusBadRequest)
		return
	}
	cliRedirectURI := cliRedirectCookie.Value
	http.SetCookie(w, &http.Cookie{Name: cliRedirectCookieName, Value: "", Path: "/", MaxAge: -1})

	if !strings.HasPrefix(cliRedirectURI, "http://localhost") {
		http.Error(w, "invalid redirect URI", http.StatusBadRequest)
		return
	}

	usermanagement.SetAPIKey(h.WorkOSAPIKey)
	authResp, err := usermanagement.AuthenticateWithCode(r.Context(), usermanagement.AuthenticateWithCodeOpts{
		ClientID: h.WorkOSClientID,
		Code:     code,
	})
	if err != nil {
		log.Printf("AuthCLICallback: WorkOS error: %v", err)
		http.Error(w, "authentication failed", http.StatusInternalServerError)
		return
	}

	userID, _, _, _, err := h.provisionUserViaAPI(r.Context(), authResp.User.Email, authResp.User.FirstName, authResp.User.LastName)
	if err != nil {
		log.Printf("AuthCLICallback: provision error: %v", err)
		http.Error(w, "failed to provision user", http.StatusInternalServerError)
		return
	}

	http.Redirect(w, r, cliRedirectURI+"?token="+userID, http.StatusFound)
}

// provisionUserViaAPI calls POST /auth/provision on the Rust API.
func (h *Handler) provisionUserViaAPI(ctx context.Context, email, firstName, lastName string) (userID, userName, slug, tier string, err error) {
	body, marshalErr := json.Marshal(map[string]string{
		"email":      email,
		"first_name": firstName,
		"last_name":  lastName,
	})
	if marshalErr != nil {
		err = fmt.Errorf("marshal provision request: %w", marshalErr)
		return
	}

	req, reqErr := http.NewRequestWithContext(ctx, http.MethodPost, h.APIURL+"/auth/provision", bytes.NewReader(body))
	if reqErr != nil {
		err = fmt.Errorf("create provision request: %w", reqErr)
		return
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("X-Internal-Secret", h.InternalSecret)

	resp, doErr := http.DefaultClient.Do(req)
	if doErr != nil {
		err = fmt.Errorf("provision request failed: %w", doErr)
		return
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		err = fmt.Errorf("provision returned %d", resp.StatusCode)
		return
	}

	var result struct {
		ID   string `json:"id"`
		Name string `json:"name"`
		Slug string `json:"slug"`
		Tier string `json:"tier"`
	}
	if decErr := json.NewDecoder(resp.Body).Decode(&result); decErr != nil {
		err = fmt.Errorf("decode provision response: %w", decErr)
		return
	}

	userID = result.ID
	userName = result.Name
	slug = result.Slug
	tier = result.Tier
	if tier == "" {
		tier = "free"
	}
	return
}

// extractSIDFromJWT decodes a JWT payload (without verifying signature) and returns the `sid` claim.
func extractSIDFromJWT(token string) (string, error) {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return "", fmt.Errorf("malformed JWT: expected 3 parts, got %d", len(parts))
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return "", fmt.Errorf("decode JWT payload: %w", err)
	}
	var claims struct {
		SID string `json:"sid"`
	}
	if err := json.Unmarshal(payload, &claims); err != nil {
		return "", fmt.Errorf("unmarshal JWT claims: %w", err)
	}
	return claims.SID, nil
}

func generateState() (string, error) {
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		return "", fmt.Errorf("generating state: %w", err)
	}
	return hex.EncodeToString(b), nil
}
