package handler

import (
	"bytes"
	"fmt"
	"os"
	"strings"

	"github.com/yuin/goldmark"
	"github.com/yuin/goldmark/extension"
	"github.com/yuin/goldmark/renderer/html"
)

var md = goldmark.New(
	goldmark.WithExtensions(extension.GFM),
	goldmark.WithRendererOptions(html.WithUnsafe()),
)

// renderDoc reads a markdown file from ui/docs/{slug}.md and returns rendered HTML.
func renderDoc(slug string) (string, error) {
	path := fmt.Sprintf("internal/ui/docs/%s.md", slug)
	src, err := os.ReadFile(path)
	if err != nil {
		return "", fmt.Errorf("read doc %q: %w", slug, err)
	}
	var buf bytes.Buffer
	if err := md.Convert(src, &buf); err != nil {
		return "", fmt.Errorf("render doc %q: %w", slug, err)
	}
	return buf.String(), nil
}

// docTitle derives a human-readable title from a doc slug.
// e.g. "getting-started/quickstart" → "Quickstart"
func docTitle(slug string) string {
	parts := strings.Split(slug, "/")
	last := parts[len(parts)-1]
	words := strings.Split(last, "-")
	for i, w := range words {
		if len(w) > 0 {
			words[i] = strings.ToUpper(w[:1]) + w[1:]
		}
	}
	return strings.Join(words, " ")
}
