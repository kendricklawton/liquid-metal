package handler

import (
	"net/http"

	"github.com/kendricklawton/liquid-metal/web/internal/ui/pages"
)

func (h *Handler) Liquid(w http.ResponseWriter, r *http.Request) {
	if h.isHTMXSwap(r, "main-content") {
		pages.LiquidContent().Render(r.Context(), w)
		return
	}
	pages.LiquidPage().Render(r.Context(), w)
}
