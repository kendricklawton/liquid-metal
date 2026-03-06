package handler

import (
	"context"
	"net/http"

	liquidmetalv1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
)

// sessionKey is the context key used to pass the user UUID through middleware.
type sessionKey string

const tokenKey sessionKey = "auth_token"

// Cookie names — HttpOnly, never exposed to JavaScript.
const (
	SessionCookieName   = "lm_session"    // authenticated user UUID
	nameCookieName      = "lm_name"       // display name for server-side rendering
	slugCookieName      = "lm_slug"       // workspace slug for URL routing
	emailCookieName     = "lm_email"      // email for account deletion confirmation
	workosUIDCookieName = "lm_workos_uid" // WorkOS user ID for account deletion
	workosSIDCookieName = "lm_workos_sid" // WorkOS session ID for logout
	tierCookieName      = "lm_tier"       // billing tier for feature gating
)

type Handler struct {
	APIURL               string
	BaseURL              string
	InternalSecret       string
	WorkOSAPIKey         string
	WorkOSClientID       string
	WorkOSRedirectURI    string
	WorkOSCLIRedirectURI string
	ServiceClient        liquidmetalv1connect.ServiceServiceClient
	UserClient           liquidmetalv1connect.UserServiceClient
}

func New(
	apiURL string,
	baseURL string,
	internalSecret string,
	workOSAPIKey string,
	workOSClientID string,
	workOSRedirectURI string,
	workOSCLIRedirectURI string,
	serviceClient liquidmetalv1connect.ServiceServiceClient,
	userClient liquidmetalv1connect.UserServiceClient,
) *Handler {
	return &Handler{
		APIURL:               apiURL,
		BaseURL:              baseURL,
		InternalSecret:       internalSecret,
		WorkOSAPIKey:         workOSAPIKey,
		WorkOSClientID:       workOSClientID,
		WorkOSRedirectURI:    workOSRedirectURI,
		WorkOSCLIRedirectURI: workOSCLIRedirectURI,
		ServiceClient:        serviceClient,
		UserClient:           userClient,
	}
}

// GetTokenFromContext extracts the user UUID set by RequireAuth.
func GetTokenFromContext(ctx context.Context) (string, bool) {
	token, ok := ctx.Value(tokenKey).(string)
	return token, ok
}

// GetDisplayName reads the display name cookie for server-side rendering.
func GetDisplayName(r *http.Request) string {
	c, err := r.Cookie(nameCookieName)
	if err != nil {
		return ""
	}
	return c.Value
}

// GetSlug reads the workspace slug cookie.
func GetSlug(r *http.Request) string {
	c, err := r.Cookie(slugCookieName)
	if err != nil {
		return ""
	}
	return c.Value
}

// GetTier reads the billing tier cookie.
func GetTier(r *http.Request) string {
	c, err := r.Cookie(tierCookieName)
	if err != nil {
		return "free"
	}
	if c.Value == "" {
		return "free"
	}
	return c.Value
}

// GetEmail reads the email cookie.
func GetEmail(r *http.Request) string {
	c, err := r.Cookie(emailCookieName)
	if err != nil {
		return ""
	}
	return c.Value
}

// RequireAuth protects routes. Redirects unauthenticated requests to /auth/login.
// For HTMX requests it sends HX-Redirect so HTMX performs a full navigation
// instead of swapping a partial into #main-content.
func (h *Handler) RequireAuth(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		cookie, err := r.Cookie(SessionCookieName)
		if err != nil || cookie.Value == "" {
			if r.Header.Get("HX-Request") == "true" {
				w.Header().Set("HX-Redirect", "/auth/login")
				w.WriteHeader(http.StatusOK)
				return
			}
			http.Redirect(w, r, "/auth/login", http.StatusFound)
			return
		}
		ctx := context.WithValue(r.Context(), tokenKey, cookie.Value)
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

func (h *Handler) isHTMXSwap(r *http.Request, target string) bool {
	return r.Header.Get("HX-Request") == "true" && r.Header.Get("HX-Target") == target
}
