package handler

import (
	"net/http"

	"github.com/kendricklawton/liquid-metal/web/internal/ui/pages"
)

func (h *Handler) Splash(w http.ResponseWriter, r *http.Request) {
	if h.isHTMXSwap(r, "main-content") {
		pages.SplashContent().Render(r.Context(), w)
		return
	}
	pages.SplashPage().Render(r.Context(), w)
}
