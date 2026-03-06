package config

import (
	"fmt"
	"os"
)

type Config struct {
	BindAddr             string
	APIURL               string
	BaseURL              string
	InternalSecret       string
	WorkOSAPIKey         string
	WorkOSClientID       string
	WorkOSRedirectURI    string
	WorkOSCLIRedirectURI string
}

func Load() (*Config, error) {
	apiURL := os.Getenv("API_URL")
	if apiURL == "" {
		return nil, fmt.Errorf("API_URL is required")
	}

	workOSAPIKey := os.Getenv("WORKOS_API_KEY")
	if workOSAPIKey == "" {
		return nil, fmt.Errorf("WORKOS_API_KEY is required")
	}

	workOSClientID := os.Getenv("WORKOS_CLIENT_ID")
	if workOSClientID == "" {
		return nil, fmt.Errorf("WORKOS_CLIENT_ID is required")
	}

	internalSecret := os.Getenv("INTERNAL_SECRET")
	if internalSecret == "" {
		return nil, fmt.Errorf("INTERNAL_SECRET is required")
	}

	bindAddr := os.Getenv("BIND_ADDR")
	if bindAddr == "" {
		bindAddr = ":3000"
	}

	baseURL := os.Getenv("BASE_URL")
	if baseURL == "" {
		baseURL = "http://localhost:3000"
	}

	redirectURI := os.Getenv("WORKOS_WEB_REDIRECT_URI")
	if redirectURI == "" {
		redirectURI = baseURL + "/auth/callback"
	}

	return &Config{
		BindAddr:          bindAddr,
		APIURL:            apiURL,
		BaseURL:           baseURL,
		InternalSecret:    internalSecret,
		WorkOSAPIKey:      workOSAPIKey,
		WorkOSClientID:    workOSClientID,
		WorkOSRedirectURI: redirectURI,
	}, nil
}
