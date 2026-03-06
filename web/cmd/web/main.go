package main

import (
	"context"
	"crypto/tls"
	"errors"
	"log"
	"net"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"connectrpc.com/connect"
	liquidmetalv1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
	"github.com/kendricklawton/liquid-metal/web/internal/config"
	"github.com/kendricklawton/liquid-metal/web/internal/handler"
	"golang.org/x/net/http2"
)

// newH2CClient returns an HTTP/2 cleartext client for talking to the Rust API.
func newH2CClient() *http.Client {
	return &http.Client{
		Transport: &http2.Transport{
			AllowHTTP: true,
			DialTLSContext: func(ctx context.Context, network, addr string, _ *tls.Config) (net.Conn, error) {
				return (&net.Dialer{}).DialContext(ctx, network, addr)
			},
		},
	}
}

func main() {
	cfg, err := config.Load()
	if err != nil {
		log.Fatalf("config: %v", err)
	}

	apiClient := newH2CClient()

	h := handler.New(
		cfg.APIURL,
		cfg.BaseURL,
		cfg.InternalSecret,
		cfg.WorkOSAPIKey,
		cfg.WorkOSClientID,
		cfg.WorkOSRedirectURI,
		cfg.WorkOSCLIRedirectURI,
		liquidmetalv1connect.NewServiceServiceClient(apiClient, cfg.APIURL, connect.WithGRPC()),
		liquidmetalv1connect.NewUserServiceClient(apiClient, cfg.APIURL, connect.WithGRPC()),
	)

	srv := &http.Server{
		Addr:    cfg.BindAddr,
		Handler: h.Routes(),
	}

	go func() {
		log.Printf("web listening on %s", cfg.BindAddr)
		if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Fatalf("web server: %v", err)
		}
	}()

	quit := make(chan os.Signal, 1)
	signal.Notify(quit, os.Interrupt, syscall.SIGTERM)
	<-quit
	log.Println("shutting down...")

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	if err := srv.Shutdown(ctx); err != nil {
		log.Fatalf("forced shutdown: %v", err)
	}

	log.Println("web server exited cleanly")
}
