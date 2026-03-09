package cmd

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"strings"

	"connectrpc.com/connect"
	"github.com/google/uuid"
	"github.com/spf13/cobra"
	"github.com/spf13/viper"

	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	v1connect "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1/liquidmetalv1connect"
)

var deployCmd = &cobra.Command{
	Use:   "deploy",
	Short: "Compile and deploy a service from flux.toml",
	RunE:  runDeploy,
}

func runDeploy(_ *cobra.Command, _ []string) error {
	t := requireToken()

	// 1. Parse flux.toml
	cfg := viper.New()
	cfg.SetConfigName("flux")
	cfg.SetConfigType("toml")
	cfg.AddConfigPath(".")
	if err := cfg.ReadInConfig(); err != nil {
		return fmt.Errorf("read flux.toml: %w (run from your project directory)", err)
	}

	name := cfg.GetString("service.name")
	engineStr := strings.ToLower(cfg.GetString("service.engine"))
	projectID := cfg.GetString("service.project_id") // Required for the new workspace hierarchy

	if name == "" {
		return fmt.Errorf("flux.toml: [service].name is required")
	}
	if projectID == "" {
		return fmt.Errorf("flux.toml: [service].project_id is required")
	}

	if engineStr != "liquid" {
		// We are building the Tracer Bullet for Wasm today
		return fmt.Errorf("only 'liquid' engine is supported in this tracer bullet")
	}

	fmt.Printf("=> Deploying %s (Engine: Liquid)...\n", name)

	// 2. Compile to WebAssembly
	fmt.Println("=> Compiling Go to WebAssembly (wasip1)...")
	wasmFile := "main.wasm"
	buildCmd := exec.Command("go", "build", "-o", wasmFile, ".")
	buildCmd.Env = append(os.Environ(), "GOOS=wasip1", "GOARCH=wasm")
	buildCmd.Stdout = os.Stdout
	buildCmd.Stderr = os.Stderr

	if err := buildCmd.Run(); err != nil {
		return fmt.Errorf("compilation failed: %w", err)
	}
	defer os.Remove(wasmFile) // Clean up artifact after deploy

	// 3. Hash the artifact
	fileBytes, err := os.ReadFile(wasmFile)
	if err != nil {
		return fmt.Errorf("failed to read %s: %w", wasmFile, err)
	}
	hash := sha256.Sum256(fileBytes)
	sha256Hex := hex.EncodeToString(hash[:])

	deployID := uuid.New().String()
	fmt.Printf("=> Artifact built: %s (SHA256: %s...)\n", wasmFile, sha256Hex[:8])

	client := v1connect.NewServiceServiceClient(newHTTPClient(), apiURL(), connect.WithGRPC())

	// 4. Request Pre-signed Upload URL
	fmt.Println("=> Requesting upload destination...")
	urlReq := withToken(connect.NewRequest(&v1.GetUploadUrlRequest{
		Slug:      name,
		Engine:    v1.Engine_ENGINE_LIQUID,
		DeployId:  deployID,
		ProjectId: projectID,
	}), t)

	urlResp, err := client.GetUploadUrl(cmdCtx(), urlReq)
	if err != nil {
		return fmt.Errorf("failed to get upload url: %w", err)
	}
	uploadUrl := urlResp.Msg.GetUploadUrl()
	artifactKey := urlResp.Msg.GetArtifactKey()

	// 5. Upload the Artifact
	fmt.Println("=> Uploading artifact to object storage...")
	httpReq, err := http.NewRequest("PUT", uploadUrl, bytes.NewReader(fileBytes))
	if err != nil {
		return fmt.Errorf("create upload request: %w", err)
	}
	// Object storage requires the exact content length
	httpReq.ContentLength = int64(len(fileBytes))

	httpResp, err := http.DefaultClient.Do(httpReq)
	if err != nil || httpResp.StatusCode >= 300 {
		if httpResp != nil {
			body, _ := io.ReadAll(httpResp.Body)
			return fmt.Errorf("upload failed (HTTP %d): %s", httpResp.StatusCode, string(body))
		}
		return fmt.Errorf("upload failed: %w", err)
	}
	httpResp.Body.Close()

	// 6. Confirm Deployment
	fmt.Println("=> Finalizing deployment...")
	deployReq := withToken(connect.NewRequest(&v1.DeployRequest{
		Name:        name,
		Slug:        name,
		Engine:      v1.Engine_ENGINE_LIQUID,
		ProjectId:   projectID,
		DeployId:    deployID,
		ArtifactKey: artifactKey,
		Sha256:      sha256Hex,
		// Using the Protobuf oneof field for Liquid
		Spec: &v1.DeployRequest_Liquid{
			Liquid: &v1.LiquidSpec{
				Entrypoint: "main.wasm",
			},
		},
	}), t)

	deployResp, err := client.Deploy(cmdCtx(), deployReq)
	if err != nil {
		return fmt.Errorf("deploy failed: %w", err)
	}

	svc := deployResp.Msg.GetService()
	fmt.Println("\n✅ Deployment Successful!")
	fmt.Printf("   Service: %s\n", svc.GetSlug())
	fmt.Printf("   Status:  %s\n", svc.GetStatus().String())

	return nil
}
