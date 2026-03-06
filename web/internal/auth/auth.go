package auth

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"time"
)

const (
	// SessionCookieName holds the authenticated user's UUID.
	SessionCookieName = "platform_session"
	// NameCookieName holds the user's display name for server-side rendering.
	NameCookieName = "platform_name"
	// SlugCookieName holds the user's workspace slug.
	SlugCookieName = "platform_slug"
	// StateCookieName is the short-lived CSRF state cookie set during login.
	StateCookieName = "auth_state"
	// WorkosSessionCookieName holds the WorkOS session ID (the `sid` JWT claim).
	WorkosSessionCookieName = "platform_workos_sid"
	// EmailCookieName holds the user's email address for server-side rendering.
	EmailCookieName = "platform_email"
	// WorkosUserIDCookieName holds the WorkOS user ID.
	WorkosUserIDCookieName = "platform_workos_uid"
	// TierCookieName holds the user's billing tier ("free", "pro", "enterprise").
	TierCookieName = "platform_tier"
	// CLIRedirectCookieName holds the CLI's local callback URI through the OAuth round-trip.
	CLIRedirectCookieName = "auth_cli_redirect"
)

// sessionKey is the context key used to pass the user UUID through middleware.
type sessionKey string

const tokenKey sessionKey = "auth_token"

// GetTokenFromContext safely extracts the user UUID from the request context.
// It is set by RequireAuth after validating the session cookie.
func GetTokenFromContext(ctx context.Context) (string, bool) {
	token, ok := ctx.Value(tokenKey).(string)
	return token, ok
}

// WithToken returns a new context with the user token set.
func WithToken(ctx context.Context, token string) context.Context {
	return context.WithValue(ctx, tokenKey, token)
}

// GetDisplayName reads the stored display name cookie for server-side rendering.
func GetDisplayName(r *http.Request) string {
	cookie, err := r.Cookie(NameCookieName)
	if err != nil || cookie.Value == "" {
		return ""
	}
	return cookie.Value
}

// GetSlug reads the workspace slug cookie for slug-scoped URL routing.
func GetSlug(r *http.Request) string {
	cookie, err := r.Cookie(SlugCookieName)
	if err != nil || cookie.Value == "" {
		return ""
	}
	return cookie.Value
}

// GetTier reads the billing tier cookie. Defaults to "free" if not set.
func GetTier(r *http.Request) string {
	cookie, err := r.Cookie(TierCookieName)
	if err != nil || cookie.Value == "" {
		return "free"
	}
	return cookie.Value
}

// GetEmail reads the stored email cookie for account confirmation flows.
func GetEmail(r *http.Request) string {
	cookie, err := r.Cookie(EmailCookieName)
	if err != nil || cookie.Value == "" {
		return ""
	}
	return cookie.Value
}

// GetWorkOSUserID reads the WorkOS user ID cookie.
func GetWorkOSUserID(r *http.Request) string {
	cookie, err := r.Cookie(WorkosUserIDCookieName)
	if err != nil || cookie.Value == "" {
		return ""
	}
	return cookie.Value
}

// ClearSessionCookies deletes all session cookies (belt-and-suspenders: MaxAge=-1 + Expires=epoch).
func ClearSessionCookies(w http.ResponseWriter) {
	past := time.Unix(0, 0)
	for _, name := range []string{
		SessionCookieName,
		NameCookieName,
		EmailCookieName,
		WorkosUserIDCookieName,
		SlugCookieName,
		WorkosSessionCookieName,
		StateCookieName,
		TierCookieName,
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
}

// GenerateState returns a cryptographically random hex string for CSRF protection.
func GenerateState() (string, error) {
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		return "", fmt.Errorf("generating state: %w", err)
	}
	return hex.EncodeToString(b), nil
}

// ExtractSIDFromJWT decodes the payload of a JWT (without verifying the
// signature) and returns the `sid` claim, which WorkOS uses as the session ID.
func ExtractSIDFromJWT(token string) (string, error) {
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
