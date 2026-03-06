package handler

import (
	"log"
	"net/http"
	"strings"
	"time"

	"connectrpc.com/connect"
	"github.com/go-chi/chi/v5"
	pb "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	"github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
	"github.com/kendricklawton/liquid-metal/web/internal/auth"
	"github.com/kendricklawton/liquid-metal/web/internal/ui/components"
	"github.com/kendricklawton/liquid-metal/web/internal/ui/pages"
	"github.com/workos/workos-go/v6/pkg/usermanagement"
)

// Handler is the Backend-For-Frontend (BFF) controller.
// It owns the WorkOS OAuth flow but delegates all DB operations to the Core API.
type Handler struct {
	APIURL               string
	BaseURL              string
	InternalSecret       string
	WorkOSAPIKey         string
	WorkOSClientID       string
	WorkOSRedirectURI    string
	WorkOSCLIRedirectURI string
	UserClient           liquidmetalv1connect.UserServiceClient
	WorkspaceClient      liquidmetalv1connect.WorkspaceServiceClient
}

// NewHandler creates a new Web Handler with all required dependencies.
func NewHandler(
	apiURL string,
	baseURL string,
	internalSecret string,
	workOSAPIKey string,
	workOSClientID string,
	workOSRedirectURI string,
	workOSCLIRedirectURI string,
	userClient liquidmetalv1connect.UserServiceClient,
	workspaceClient liquidmetalv1connect.WorkspaceServiceClient,
) *Handler {
	return &Handler{
		APIURL:               apiURL,
		BaseURL:              baseURL,
		InternalSecret:       internalSecret,
		WorkOSAPIKey:         workOSAPIKey,
		WorkOSClientID:       workOSClientID,
		WorkOSRedirectURI:    workOSRedirectURI,
		WorkOSCLIRedirectURI: workOSCLIRedirectURI,
		UserClient:           userClient,
		WorkspaceClient:      workspaceClient,
	}
}

// isMainContentSwap reports whether the request is an HTMX partial swap targeting #main-content.
func (h *Handler) isMainContentSwap(r *http.Request) bool {
	return r.Header.Get("HX-Request") == "true" && r.Header.Get("HX-Target") == "main-content"
}

// isDashboardSwap reports whether the request is an HTMX partial swap targeting #dashboard-content.
func (h *Handler) isDashboardSwap(r *http.Request) bool {
	return r.Header.Get("HX-Request") == "true" && r.Header.Get("HX-Target") == "dashboard-content"
}

// dashboardAuth validates auth and returns userName; redirects and returns "" on failure.
func (h *Handler) dashboardAuth(w http.ResponseWriter, r *http.Request) string {
	_, ok := auth.GetTokenFromContext(r.Context())
	if !ok {
		http.Redirect(w, r, "/auth/login", http.StatusFound)
		return ""
	}
	return auth.GetDisplayName(r)
}

// dashboardSlug validates auth and the URL slug against the user's slug cookie.
// Returns ("", "") and handles the redirect itself on any failure.
func (h *Handler) dashboardSlug(w http.ResponseWriter, r *http.Request) (userName, slug string) {
	userName = h.dashboardAuth(w, r)
	if userName == "" {
		return
	}
	slug = chi.URLParam(r, "slug")
	cookieSlug := auth.GetSlug(r)
	if slug != cookieSlug {
		http.Redirect(w, r, "/"+cookieSlug, http.StatusFound)
		slug = ""
	}
	return
}

// DashboardRedirect resolves /dashboard → /{slug} using the slug cookie.
func (h *Handler) DashboardRedirect(w http.ResponseWriter, r *http.Request) {
	slug := auth.GetSlug(r)
	if slug == "" {
		http.Redirect(w, r, "/auth/login", http.StatusFound)
		return
	}
	http.Redirect(w, r, "/"+slug, http.StatusFound)
}

// Splash renders the home page.
func (h *Handler) Splash(w http.ResponseWriter, r *http.Request) {
	// Redirect logged-in users straight to their workspace.
	// Only redirect when both session and slug cookies are present —
	// partial/stale state falls through to render the splash page.
	if _, err := r.Cookie(auth.SessionCookieName); err == nil {
		if slug := auth.GetSlug(r); slug != "" {
			http.Redirect(w, r, "/"+slug, http.StatusFound)
			return
		}
	}
	userName := auth.GetDisplayName(r)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")

	if h.isMainContentSwap(r) {
		component := pages.SplashContent("INITIALIZING PLATFORM...", userName)
		if err := component.Render(r.Context(), w); err != nil {
			http.Error(w, "render error", http.StatusInternalServerError)
		}
		return
	}

	component := pages.SplashPage("INITIALIZING PLATFORM...", userName)
	if err := component.Render(r.Context(), w); err != nil {
		http.Error(w, "render error", http.StatusInternalServerError)
	}
}

// Templates renders the templates page.
func (h *Handler) Templates(w http.ResponseWriter, r *http.Request) {
	userName := auth.GetDisplayName(r)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")

	if h.isMainContentSwap(r) {
		component := pages.TemplatesContent(userName)
		if err := component.Render(r.Context(), w); err != nil {
			http.Error(w, "render error", http.StatusInternalServerError)
		}
		return
	}

	component := pages.TemplatesPage(userName)
	if err := component.Render(r.Context(), w); err != nil {
		http.Error(w, "render error", http.StatusInternalServerError)
	}
}

// TemplateDetail renders a template detail page.
func (h *Handler) TemplateDetail(w http.ResponseWriter, r *http.Request) {
	userName := auth.GetDisplayName(r)
	slug := chi.URLParam(r, "slug")
	w.Header().Set("Content-Type", "text/html; charset=utf-8")

	if h.isMainContentSwap(r) {
		component := pages.TemplateDetailContent(userName, slug)
		if err := component.Render(r.Context(), w); err != nil {
			http.Error(w, "render error", http.StatusInternalServerError)
		}
		return
	}

	component := pages.TemplateDetailPage(userName, slug)
	if err := component.Render(r.Context(), w); err != nil {
		http.Error(w, "render error", http.StatusInternalServerError)
	}
}

// Changelog renders the changelog page.
func (h *Handler) Changelog(w http.ResponseWriter, r *http.Request) {
	userName := auth.GetDisplayName(r)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")

	if h.isMainContentSwap(r) {
		component := pages.ChangelogContent(userName)
		if err := component.Render(r.Context(), w); err != nil {
			http.Error(w, "render error", http.StatusInternalServerError)
		}
		return
	}

	component := pages.ChangelogPage(userName)
	if err := component.Render(r.Context(), w); err != nil {
		http.Error(w, "render error", http.StatusInternalServerError)
	}
}

// About renders the about page.
func (h *Handler) About(w http.ResponseWriter, r *http.Request) {
	userName := auth.GetDisplayName(r)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")

	if h.isMainContentSwap(r) {
		component := pages.AboutContent(userName)
		if err := component.Render(r.Context(), w); err != nil {
			http.Error(w, "render error", http.StatusInternalServerError)
		}
		return
	}

	component := pages.AboutPage(userName)
	if err := component.Render(r.Context(), w); err != nil {
		http.Error(w, "render error", http.StatusInternalServerError)
	}
}

// Pricing renders the pricing page.
func (h *Handler) Pricing(w http.ResponseWriter, r *http.Request) {
	userName := auth.GetDisplayName(r)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")

	if h.isMainContentSwap(r) {
		component := pages.PricingContent(userName)
		if err := component.Render(r.Context(), w); err != nil {
			http.Error(w, "render error", http.StatusInternalServerError)
		}
		return
	}

	component := pages.PricingPage(userName)
	if err := component.Render(r.Context(), w); err != nil {
		http.Error(w, "render error", http.StatusInternalServerError)
	}
}

// Healthz handles the connection status check and returns the action button fragment.
func (h *Handler) Healthz(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")

	_, err := r.Cookie(auth.SessionCookieName)
	isLoggedIn := err == nil

	actionURL := "/auth/login"
	buttonText := "INITIALIZE LOGIN SEQUENCE"
	if isLoggedIn {
		slug := auth.GetSlug(r)
		if slug == "" {
			slug = "dashboard"
		}
		actionURL = "/" + slug
		buttonText = "ENTER SECURE CONSOLE"
	}

	component := components.HealthzStatus(actionURL, buttonText)
	if err := component.Render(r.Context(), w); err != nil {
		http.Error(w, "render error", http.StatusInternalServerError)
	}
}

// Docs renders a documentation page by slug.
func (h *Handler) Docs(w http.ResponseWriter, r *http.Request) {
	slug := strings.TrimPrefix(r.URL.Path, "/docs/")
	slug = strings.TrimSuffix(slug, "/")
	if slug == "" || slug == "docs" {
		http.Redirect(w, r, "/docs/getting-started/quickstart", http.StatusFound)
		return
	}

	content, err := renderDoc(slug)
	if err != nil {
		http.NotFound(w, r)
		return
	}

	title := docTitle(slug)
	userName := auth.GetDisplayName(r)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")

	if h.isMainContentSwap(r) {
		component := pages.DocsContent(slug, title, content)
		if err := component.Render(r.Context(), w); err != nil {
			http.Error(w, "render error", http.StatusInternalServerError)
		}
		return
	}

	component := pages.DocsPage(slug, title, content, userName)
	if err := component.Render(r.Context(), w); err != nil {
		http.Error(w, "render error", http.StatusInternalServerError)
	}
}

// Dashboard renders the projects overview. Requires RequireAuth middleware.
func (h *Handler) Dashboard(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	// Coming from the public-site HTMX nav — force a full page load into the app shell.
	if h.isMainContentSwap(r) {
		w.Header().Set("HX-Redirect", "/"+slug)
		return
	}
	if h.isDashboardSwap(r) {
		pages.DashboardContent(userName, slug).Render(r.Context(), w)
		return
	}
	pages.DashboardPage(userName, slug).Render(r.Context(), w)
}

// Project renders the project overview page.
func (h *Handler) Project(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	projectID := chi.URLParam(r, "projectID")
	projectName := projectID
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.ProjectContent(slug, projectID, projectName).Render(r.Context(), w)
		return
	}
	pages.ProjectPage(userName, slug, projectID, projectName).Render(r.Context(), w)
}

// DashboardServices renders the services page.
func (h *Handler) DashboardServices(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardServicesContent().Render(r.Context(), w)
		return
	}
	pages.DashboardServicesPage(userName, slug).Render(r.Context(), w)
}

// DashboardDeployments renders the deployments page.
func (h *Handler) DashboardDeployments(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardDeploymentsContent().Render(r.Context(), w)
		return
	}
	pages.DashboardDeploymentsPage(userName, slug).Render(r.Context(), w)
}

// DashboardLogs renders the logs page.
func (h *Handler) DashboardLogs(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardLogsContent().Render(r.Context(), w)
		return
	}
	pages.DashboardLogsPage(userName, slug).Render(r.Context(), w)
}

// DashboardSecrets renders the secrets page.
func (h *Handler) DashboardSecrets(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardSecretsContent().Render(r.Context(), w)
		return
	}
	pages.DashboardSecretsPage(userName, slug).Render(r.Context(), w)
}

// DashboardDomains renders the domains page.
func (h *Handler) DashboardDomains(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardDomainsContent().Render(r.Context(), w)
		return
	}
	pages.DashboardDomainsPage(userName, slug).Render(r.Context(), w)
}

// DashboardWebhooks renders the webhooks page.
func (h *Handler) DashboardWebhooks(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardWebhooksContent().Render(r.Context(), w)
		return
	}
	pages.DashboardWebhooksPage(userName, slug).Render(r.Context(), w)
}

// DashboardSettings renders the dashboard settings page.
func (h *Handler) DashboardSettings(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardSettingsContent().Render(r.Context(), w)
		return
	}
	pages.DashboardSettingsPage(userName, slug).Render(r.Context(), w)
}

// DashboardBilling renders the billing page.
func (h *Handler) DashboardBilling(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardBillingContent(slug).Render(r.Context(), w)
		return
	}
	pages.DashboardBillingPage(userName, slug).Render(r.Context(), w)
}

// DashboardBlueprints renders the blueprints page.
func (h *Handler) DashboardBlueprints(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardBlueprintsContent().Render(r.Context(), w)
		return
	}
	pages.DashboardBlueprintsPage(userName, slug).Render(r.Context(), w)
}

// DashboardEnvGroups renders the environment groups page.
func (h *Handler) DashboardEnvGroups(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardEnvGroupsContent().Render(r.Context(), w)
		return
	}
	pages.DashboardEnvGroupsPage(userName, slug).Render(r.Context(), w)
}

// DashboardObservability renders the observability page.
func (h *Handler) DashboardObservability(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardObservabilityContent().Render(r.Context(), w)
		return
	}
	pages.DashboardObservabilityPage(userName, slug).Render(r.Context(), w)
}

// DashboardNotifications renders the notifications page.
func (h *Handler) DashboardNotifications(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardNotificationsContent().Render(r.Context(), w)
		return
	}
	pages.DashboardNotificationsPage(userName, slug).Render(r.Context(), w)
}

// DashboardPrivateLinks renders the private links page.
func (h *Handler) DashboardPrivateLinks(w http.ResponseWriter, r *http.Request) {
	userName, slug := h.dashboardSlug(w, r)
	if slug == "" {
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.DashboardPrivateLinksContent().Render(r.Context(), w)
		return
	}
	pages.DashboardPrivateLinksPage(userName, slug).Render(r.Context(), w)
}

// Account renders the account settings page. Requires RequireAuth middleware.
func (h *Handler) Account(w http.ResponseWriter, r *http.Request) {
	userName := auth.GetDisplayName(r)
	email := auth.GetEmail(r)
	slug := auth.GetSlug(r)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.AccountContent(userName, email, slug).Render(r.Context(), w)
		return
	}
	pages.AccountPage(userName, email, slug).Render(r.Context(), w)
}

// AccountDelete deletes the authenticated user's account, clears all cookies,
// revokes the WorkOS session, and redirects to the splash page.
// Requires the user to confirm by typing their email address.
func (h *Handler) AccountDelete(w http.ResponseWriter, r *http.Request) {
	userID, ok := auth.GetTokenFromContext(r.Context())
	if !ok || userID == "" {
		http.Redirect(w, r, "/auth/login", http.StatusFound)
		return
	}
	// Require email confirmation matching the stored email cookie
	expected := auth.GetEmail(r)
	if expected == "" || r.FormValue("confirm") != expected {
		http.Error(w, "Confirmation does not match your email address", http.StatusBadRequest)
		return
	}
	if err := h.deleteAccountViaAPI(r.Context(), userID); err != nil {
		log.Printf("AccountDelete error: %v", err)
		http.Error(w, "Failed to delete account", http.StatusInternalServerError)
		return
	}
	// Delete the user from WorkOS so they can't re-authenticate without re-registering
	usermanagement.SetAPIKey(h.WorkOSAPIKey)
	if wosUID := auth.GetWorkOSUserID(r); wosUID != "" {
		if err := usermanagement.DeleteUser(r.Context(), usermanagement.DeleteUserOpts{User: wosUID}); err != nil {
			log.Printf("AccountDelete: failed to delete WorkOS user %s: %v", wosUID, err)
		}
	}
	// Clear all session cookies
	auth.ClearSessionCookies(w)
	// Redirect home — WorkOS session is already gone since we deleted the user
	w.Header().Set("HX-Redirect", "/")
	w.WriteHeader(http.StatusOK)
}

// Settings renders the protected settings page. Requires RequireAuth middleware.
func (h *Handler) Settings(w http.ResponseWriter, r *http.Request) {
	userName := auth.GetDisplayName(r)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")

	if h.isMainContentSwap(r) {
		component := pages.SettingsContent()
		if err := component.Render(r.Context(), w); err != nil {
			http.Error(w, "render error", http.StatusInternalServerError)
		}
		return
	}

	component := pages.SettingsPage(userName)
	if err := component.Render(r.Context(), w); err != nil {
		http.Error(w, "render error", http.StatusInternalServerError)
	}
}

// NewWorkspace renders the create-workspace page, or the upgrade gate for free users who already have a workspace.
func (h *Handler) NewWorkspace(w http.ResponseWriter, r *http.Request) {
	userName := h.dashboardAuth(w, r)
	if userName == "" {
		return
	}
	slug := auth.GetSlug(r)
	gated := auth.GetTier(r) == "free" && slug != ""
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if h.isDashboardSwap(r) {
		pages.NewWorkspaceContent(slug, gated, "").Render(r.Context(), w)
		return
	}
	pages.NewWorkspacePage(userName, slug, gated).Render(r.Context(), w)
}

// NewWorkspacePost handles the create-workspace form submission.
func (h *Handler) NewWorkspacePost(w http.ResponseWriter, r *http.Request) {
	userName := h.dashboardAuth(w, r)
	if userName == "" {
		return
	}
	slug := auth.GetSlug(r)
	if err := r.ParseForm(); err != nil {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		pages.NewWorkspaceContent(slug, false, "Invalid form submission.").Render(r.Context(), w)
		return
	}
	name := strings.TrimSpace(r.FormValue("name"))
	newSlug := strings.TrimSpace(r.FormValue("slug"))
	if name == "" || newSlug == "" {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		pages.NewWorkspaceContent(slug, false, "Workspace name and URL are required.").Render(r.Context(), w)
		return
	}
	userID, ok := auth.GetTokenFromContext(r.Context())
	if !ok {
		http.Redirect(w, r, "/auth/login", http.StatusFound)
		return
	}
	req := connect.NewRequest(&pb.CreateWorkspaceRequest{Name: name, Slug: newSlug})
	req.Header().Set("Authorization", "Bearer "+userID)
	resp, err := h.WorkspaceClient.CreateWorkspace(r.Context(), req)
	if err != nil {
		log.Printf("NewWorkspacePost: CreateWorkspace error: %v", err)
		errMsg := "Failed to create workspace. The URL may already be taken."
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		pages.NewWorkspaceContent(slug, false, errMsg).Render(r.Context(), w)
		return
	}
	createdSlug := resp.Msg.GetSlug()
	// Update slug cookie to the new workspace
	http.SetCookie(w, &http.Cookie{
		Name:     auth.SlugCookieName,
		Value:    createdSlug,
		Path:     "/",
		HttpOnly: true,
		SameSite: http.SameSiteLaxMode,
		Expires:  time.Now().Add(7 * 24 * time.Hour),
	})
	w.Header().Set("HX-Redirect", "/"+createdSlug)
	w.WriteHeader(http.StatusOK)
}
