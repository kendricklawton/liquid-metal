package handler

import (
	"net/http"

	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"
	"golang.org/x/net/http2"
	"golang.org/x/net/http2/h2c"
)

// Routes builds and returns the full HTTP handler for the web server.
func (h *Handler) Routes() http.Handler {
	r := chi.NewRouter()
	r.Use(middleware.Logger)
	r.Use(middleware.Recoverer)

	r.Handle("/static/*", http.StripPrefix("/static/", http.FileServer(http.Dir("internal/ui/static"))))

	// Public routes
	r.Get("/", h.Splash)
	r.Get("/metal", h.Metal)
	r.Get("/liquid", h.Liquid)

	// Auth — full page navigations only, never HTMX partial swaps
	r.Route("/auth", func(auth chi.Router) {
		auth.Get("/login", h.AuthLogin)
		auth.Get("/callback", h.AuthCallback)
		auth.Get("/logout", h.AuthLogout)
		auth.Post("/logout", h.AuthLogout)
		auth.Get("/cli/login", h.AuthCLILogin)
		auth.Get("/cli/callback", h.AuthCLICallback)
	})

	// Protected app routes — require valid lm_session cookie
	r.Group(func(app chi.Router) {
		app.Use(h.RequireAuth)

		// /account — user profile
		app.Get("/account", h.AccountPage)

		// /{slug} — workspace root: services list
		// /{slug}/billing, /{slug}/settings
		app.Get("/{slug}", h.ServicesPage)
		app.Get("/{slug}/billing", h.BillingPage)
		app.Get("/{slug}/settings", h.SettingsPage)
	})

	return h2c.NewHandler(r, &http2.Server{})
}
