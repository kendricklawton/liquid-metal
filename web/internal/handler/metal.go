package handler

import (
	"net/http"

	"github.com/kendricklawton/liquid-metal/web/internal/ui/pages"
)

func (h *Handler) Metal(w http.ResponseWriter, r *http.Request) {
	if h.isHTMXSwap(r, "main-content") {
		pages.MetalContent().Render(r.Context(), w)
		return
	}
	pages.MetalPage().Render(r.Context(), w)
}
