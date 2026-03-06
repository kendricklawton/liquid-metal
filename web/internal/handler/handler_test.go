package handler

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

// newTestHandler builds a Handler with empty config — sufficient for tests
// that only exercise cookie helpers and middleware (no ConnectRPC calls).
func newTestHandler() *Handler {
	return New("", "", "", "", "", "", "", nil, nil)
}

// ── Cookie helpers ────────────────────────────────────────────────────────────

func TestGetDisplayName(t *testing.T) {
	tests := []struct {
		name     string
		cookie   *http.Cookie
		expected string
	}{
		{"with cookie", &http.Cookie{Name: nameCookieName, Value: "Alice"}, "Alice"},
		{"missing cookie", nil, ""},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			r := httptest.NewRequest(http.MethodGet, "/", nil)
			if tc.cookie != nil {
				r.AddCookie(tc.cookie)
			}
			if got := GetDisplayName(r); got != tc.expected {
				t.Errorf("GetDisplayName() = %q, want %q", got, tc.expected)
			}
		})
	}
}

func TestGetSlug(t *testing.T) {
	tests := []struct {
		name     string
		cookie   *http.Cookie
		expected string
	}{
		{"with cookie", &http.Cookie{Name: slugCookieName, Value: "acme"}, "acme"},
		{"missing cookie", nil, ""},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			r := httptest.NewRequest(http.MethodGet, "/", nil)
			if tc.cookie != nil {
				r.AddCookie(tc.cookie)
			}
			if got := GetSlug(r); got != tc.expected {
				t.Errorf("GetSlug() = %q, want %q", got, tc.expected)
			}
		})
	}
}

func TestGetTier(t *testing.T) {
	tests := []struct {
		name     string
		cookie   *http.Cookie
		expected string
	}{
		{"with tier", &http.Cookie{Name: tierCookieName, Value: "pro"}, "pro"},
		{"empty value defaults to free", &http.Cookie{Name: tierCookieName, Value: ""}, "free"},
		{"missing cookie defaults to free", nil, "free"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			r := httptest.NewRequest(http.MethodGet, "/", nil)
			if tc.cookie != nil {
				r.AddCookie(tc.cookie)
			}
			if got := GetTier(r); got != tc.expected {
				t.Errorf("GetTier() = %q, want %q", got, tc.expected)
			}
		})
	}
}

func TestGetEmail(t *testing.T) {
	tests := []struct {
		name     string
		cookie   *http.Cookie
		expected string
	}{
		{"with email", &http.Cookie{Name: emailCookieName, Value: "alice@example.com"}, "alice@example.com"},
		{"missing cookie", nil, ""},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			r := httptest.NewRequest(http.MethodGet, "/", nil)
			if tc.cookie != nil {
				r.AddCookie(tc.cookie)
			}
			if got := GetEmail(r); got != tc.expected {
				t.Errorf("GetEmail() = %q, want %q", got, tc.expected)
			}
		})
	}
}

// ── RequireAuth middleware ─────────────────────────────────────────────────────

func TestRequireAuth_RedirectsUnauthenticated(t *testing.T) {
	h := newTestHandler()
	next := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
	})

	r := httptest.NewRequest(http.MethodGet, "/acme", nil)
	w := httptest.NewRecorder()
	h.RequireAuth(next).ServeHTTP(w, r)

	if w.Code != http.StatusFound {
		t.Errorf("status = %d, want %d", w.Code, http.StatusFound)
	}
	loc := w.Header().Get("Location")
	if loc != "/auth/login" {
		t.Errorf("Location = %q, want %q", loc, "/auth/login")
	}
}

func TestRequireAuth_HTMXSendsHXRedirect(t *testing.T) {
	h := newTestHandler()
	next := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
	})

	r := httptest.NewRequest(http.MethodGet, "/acme", nil)
	r.Header.Set("HX-Request", "true")
	w := httptest.NewRecorder()
	h.RequireAuth(next).ServeHTTP(w, r)

	if w.Code != http.StatusOK {
		t.Errorf("status = %d, want 200 (HTMX uses HX-Redirect, not 302)", w.Code)
	}
	if got := w.Header().Get("HX-Redirect"); got != "/auth/login" {
		t.Errorf("HX-Redirect = %q, want %q", got, "/auth/login")
	}
}

func TestRequireAuth_PassesAuthenticatedRequest(t *testing.T) {
	h := newTestHandler()

	var capturedToken string
	next := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		capturedToken, _ = GetTokenFromContext(r.Context())
		w.WriteHeader(http.StatusOK)
	})

	r := httptest.NewRequest(http.MethodGet, "/acme", nil)
	r.AddCookie(&http.Cookie{Name: SessionCookieName, Value: "user-uuid-123"})
	w := httptest.NewRecorder()
	h.RequireAuth(next).ServeHTTP(w, r)

	if w.Code != http.StatusOK {
		t.Errorf("status = %d, want 200", w.Code)
	}
	if capturedToken != "user-uuid-123" {
		t.Errorf("token in context = %q, want %q", capturedToken, "user-uuid-123")
	}
}

// ── isHTMXSwap ────────────────────────────────────────────────────────────────

func TestIsHTMXSwap(t *testing.T) {
	h := newTestHandler()

	tests := []struct {
		name      string
		hxRequest string
		hxTarget  string
		target    string
		want      bool
	}{
		{"htmx + matching target", "true", "app-content", "app-content", true},
		{"htmx + wrong target", "true", "other", "app-content", false},
		{"not htmx", "", "app-content", "app-content", false},
		{"htmx but no target", "true", "", "app-content", false},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			r := httptest.NewRequest(http.MethodGet, "/", nil)
			if tc.hxRequest != "" {
				r.Header.Set("HX-Request", tc.hxRequest)
			}
			if tc.hxTarget != "" {
				r.Header.Set("HX-Target", tc.hxTarget)
			}
			if got := h.isHTMXSwap(r, tc.target); got != tc.want {
				t.Errorf("isHTMXSwap() = %v, want %v", got, tc.want)
			}
		})
	}
}
