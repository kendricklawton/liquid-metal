package handler

import (
	"log"
	"net/http"

	"connectrpc.com/connect"
	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	"github.com/kendricklawton/liquid-metal/web/internal/ui/pages"
)

// ServicesPage lists all services for the authenticated user's workspace.
func (h *Handler) ServicesPage(w http.ResponseWriter, r *http.Request) {
	userName := GetDisplayName(r)
	slug := GetSlug(r)
	tier := GetTier(r)

	token, _ := GetTokenFromContext(r.Context())
	req := connect.NewRequest(&v1.ListServicesRequest{})
	req.Header().Set("Authorization", "Bearer "+token)

	resp, err := h.ServiceClient.ListServices(r.Context(), req)
	if err != nil {
		log.Printf("ListServices error: %v", err)
		// Render page with empty list rather than a hard error
		if h.isHTMXSwap(r, "app-content") {
			pages.ServicesContent(slug, nil).Render(r.Context(), w)
		} else {
			pages.ServicesPage(userName, slug, tier, nil).Render(r.Context(), w)
		}
		return
	}

	svcs := resp.Msg.GetServices()
	if h.isHTMXSwap(r, "app-content") {
		pages.ServicesContent(slug, svcs).Render(r.Context(), w)
		return
	}
	pages.ServicesPage(userName, slug, tier, svcs).Render(r.Context(), w)
}

// BillingPage renders the billing/usage page.
func (h *Handler) BillingPage(w http.ResponseWriter, r *http.Request) {
	userName := GetDisplayName(r)
	slug := GetSlug(r)
	tier := GetTier(r)

	if h.isHTMXSwap(r, "app-content") {
		pages.BillingContent().Render(r.Context(), w)
		return
	}
	pages.BillingPage(userName, slug, tier).Render(r.Context(), w)
}

// SettingsPage renders the workspace settings page.
func (h *Handler) SettingsPage(w http.ResponseWriter, r *http.Request) {
	userName := GetDisplayName(r)
	slug := GetSlug(r)
	tier := GetTier(r)

	if h.isHTMXSwap(r, "app-content") {
		pages.SettingsContent().Render(r.Context(), w)
		return
	}
	pages.SettingsPage(userName, slug, tier).Render(r.Context(), w)
}

// AccountPage renders the user account page.
func (h *Handler) AccountPage(w http.ResponseWriter, r *http.Request) {
	userName := GetDisplayName(r)
	slug := GetSlug(r)
	tier := GetTier(r)
	email := GetEmail(r)

	if h.isHTMXSwap(r, "app-content") {
		pages.AccountContent(userName, email).Render(r.Context(), w)
		return
	}
	pages.AccountPage(userName, slug, tier, email).Render(r.Context(), w)
}
