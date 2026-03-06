package mw

import (
	"context"
	"net/http"
)

// We use a custom type for context keys to prevent collisions with other packages
type contextKey string

const htmxKey contextKey = "isHTMX"

// HTMX checks for the HX-Request header and attaches the result to the request context.
func HTMX(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// HTMX requests always include this header
		isHTMX := r.Header.Get("HX-Request") == "true"

		// Create a new context with the boolean value
		ctx := context.WithValue(r.Context(), htmxKey, isHTMX)

		// Pass the new context down the chain to the actual handler
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// IsHTMX is a tiny helper function to extract the boolean from the context.
// You will call this inside your route handlers.
func IsHTMX(r *http.Request) bool {
	if val, ok := r.Context().Value(htmxKey).(bool); ok {
		return val
	}
	return false
}
