package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"strings"
	"time"

	"github.com/kendricklawton/liquid-metal/web/internal/auth"
	"github.com/kendricklawton/liquid-metal/web/internal/mw"
	"github.com/workos/workos-go/v6/pkg/usermanagement"
)

// RequireAuth protects routes by checking for a valid session cookie.
func (h *Handler) RequireAuth(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		cookie, err := r.Cookie(auth.SessionCookieName)

		if err != nil || cookie.Value == "" {
			// The clean HTMX check!
			if mw.IsHTMX(r) {
				w.Header().Set("HX-Redirect", "/auth/login")
				w.WriteHeader(http.StatusOK)
				return
			}
			http.Redirect(w, r, "/auth/login", http.StatusFound)
			return
		}

		ctx := auth.WithToken(r.Context(), cookie.Value)
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// AuthLogin generates a CSRF state, stores it in a short-lived cookie,
// then redirects the browser to WorkOS AuthKit.
func (h *Handler) AuthLogin(w http.ResponseWriter, r *http.Request) {
	state, err := auth.GenerateState()
	if err != nil {
		http.Error(w, "Failed to initialize login", http.StatusInternalServerError)
		return
	}

	http.SetCookie(w, &http.Cookie{
		Name:     auth.StateCookieName,
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

// AuthCallback handles the return redirect from WorkOS.
func (h *Handler) AuthCallback(w http.ResponseWriter, r *http.Request) {
	code := r.URL.Query().Get("code")
	returnedState := r.URL.Query().Get("state")

	stateCookie, err := r.Cookie(auth.StateCookieName)
	if err != nil || returnedState == "" || stateCookie.Value != returnedState {
		http.Error(w, "Invalid or expired authentication state", http.StatusUnauthorized)
		return
	}
	// Clear state cookie immediately after verification
	http.SetCookie(w, &http.Cookie{Name: auth.StateCookieName, Value: "", Path: "/", MaxAge: -1})

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

	// Grouped session cookie creation for readability
	exp := time.Now().Add(7 * 24 * time.Hour)
	setSessionCookie := func(name, value string) {
		http.SetCookie(w, &http.Cookie{
			Name:     name,
			Value:    value,
			Path:     "/",
			HttpOnly: true,
			SameSite: http.SameSiteLaxMode,
			Expires:  exp,
		})
	}

	setSessionCookie(auth.SessionCookieName, userID)
	setSessionCookie(auth.NameCookieName, userName)
	setSessionCookie(auth.EmailCookieName, authResp.User.Email)
	setSessionCookie(auth.WorkosUserIDCookieName, authResp.User.ID)
	setSessionCookie(auth.SlugCookieName, workspaceSlug)
	setSessionCookie(auth.TierCookieName, tier)

	if sid, err := auth.ExtractSIDFromJWT(authResp.AccessToken); err == nil && sid != "" {
		setSessionCookie(auth.WorkosSessionCookieName, sid)
	} else if err != nil {
		log.Printf("AuthCallback: could not extract WorkOS session ID: %v", err)
	}

	if workspaceSlug == "" {
		http.Redirect(w, r, "/dashboard", http.StatusFound)
		return
	}
	http.Redirect(w, r, "/"+workspaceSlug, http.StatusFound)
}

// AuthLogout clears all session cookies and redirects through WorkOS logout.
func (h *Handler) AuthLogout(w http.ResponseWriter, r *http.Request) {
	log.Printf("AuthLogout: clearing session for %s", r.RemoteAddr)

	sidCookie, _ := r.Cookie(auth.WorkosSessionCookieName)
	auth.ClearSessionCookies(w)

	var redirectURL string

	if sidCookie != nil && sidCookie.Value != "" {
		usermanagement.SetAPIKey(h.WorkOSAPIKey)
		logoutURL, err := usermanagement.GetLogoutURL(usermanagement.GetLogoutURLOpts{
			SessionID: sidCookie.Value,
			ReturnTo:  h.BaseURL,
		})
		if err == nil {
			redirectURL = logoutURL.String()
		} else {
			log.Printf("AuthLogout: failed to build WorkOS logout URL: %v", err)
			redirectURL = h.BaseURL
		}
	} else {
		redirectURL = h.BaseURL
	}

	// Make logout HTMX-safe just in case it's triggered by an hx-post button
	if mw.IsHTMX(r) {
		w.Header().Set("HX-Redirect", redirectURL)
		w.WriteHeader(http.StatusOK)
		return
	}

	http.Redirect(w, r, redirectURL, http.StatusSeeOther)
}

// AuthCLILogin starts the browser-based OAuth flow for the CLI.
func (h *Handler) AuthCLILogin(w http.ResponseWriter, r *http.Request) {
	cliRedirectURI := r.URL.Query().Get("redirect_uri")
	if cliRedirectURI == "" || !strings.HasPrefix(cliRedirectURI, "http://localhost") {
		http.Error(w, "invalid redirect_uri: must be http://localhost", http.StatusBadRequest)
		return
	}

	state, err := auth.GenerateState()
	if err != nil {
		http.Error(w, "failed to initialize login", http.StatusInternalServerError)
		return
	}

	http.SetCookie(w, &http.Cookie{Name: auth.StateCookieName, Value: state, Path: "/", HttpOnly: true, SameSite: http.SameSiteLaxMode, MaxAge: 300})
	http.SetCookie(w, &http.Cookie{Name: auth.CLIRedirectCookieName, Value: cliRedirectURI, Path: "/", HttpOnly: true, SameSite: http.SameSiteLaxMode, MaxAge: 300})

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

// AuthCLICallback handles the WorkOS redirect for the CLI auth flow.
func (h *Handler) AuthCLICallback(w http.ResponseWriter, r *http.Request) {
	code := r.URL.Query().Get("code")
	returnedState := r.URL.Query().Get("state")

	stateCookie, err := r.Cookie(auth.StateCookieName)
	if err != nil || returnedState == "" || stateCookie.Value != returnedState {
		http.Error(w, "invalid or expired authentication state", http.StatusUnauthorized)
		return
	}
	http.SetCookie(w, &http.Cookie{Name: auth.StateCookieName, Value: "", Path: "/", MaxAge: -1})

	cliRedirectCookie, err := r.Cookie(auth.CLIRedirectCookieName)
	if err != nil || cliRedirectCookie.Value == "" {
		http.Error(w, "missing CLI redirect URI", http.StatusBadRequest)
		return
	}
	cliRedirectURI := cliRedirectCookie.Value
	http.SetCookie(w, &http.Cookie{Name: auth.CLIRedirectCookieName, Value: "", Path: "/", MaxAge: -1})

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

// provisionUserViaAPI calls POST /v1/auth/provision on the Core API.
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

	req, reqErr := http.NewRequestWithContext(ctx, http.MethodPost, h.APIURL+"/v1/auth/provision", bytes.NewReader(body))
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
		tier = "free" // Default to free if the core API doesn't specify
	}
	return
}

// deleteAccountViaAPI calls DELETE /v1/auth/account on the Core API.
func (h *Handler) deleteAccountViaAPI(ctx context.Context, userID string) error {
	body, err := json.Marshal(map[string]string{"user_id": userID})
	if err != nil {
		return fmt.Errorf("marshal delete request: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodDelete, h.APIURL+"/v1/auth/account", bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("create delete request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("X-Internal-Secret", h.InternalSecret)

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return fmt.Errorf("delete request failed: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusNoContent {
		return fmt.Errorf("delete account returned %d", resp.StatusCode)
	}
	return nil
}
