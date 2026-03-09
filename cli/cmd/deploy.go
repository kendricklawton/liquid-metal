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
	Short: "Build and deploy the service in the current directory",
	RunE:  runDeploy,
}

func runDeploy(_ *cobra.Command, _ []string) error {
	t := requireToken()

	// 1. Parse liquid-metal.toml — auto-init if not present
	cfg := viper.New()
	cfg.SetConfigName("liquid-metal")
	cfg.SetConfigType("toml")
	cfg.AddConfigPath(".")

	if err := cfg.ReadInConfig(); err != nil {
		if !isConfigNotFound(err) {
			return fmt.Errorf("read liquid-metal.toml: %w", err)
		}
		return fmt.Errorf("no liquid-metal.toml found\n\nRun `flux init` to set up this directory as a Liquid Metal service.")
	}

	name := cfg.GetString("service.name")
	engineStr := strings.ToLower(cfg.GetString("service.engine"))
	projectID := cfg.GetString("service.project_id")

	if name == "" {
		return fmt.Errorf("liquid-metal.toml: [service].name is required")
	}
	if projectID == "" {
		return fmt.Errorf("liquid-metal.toml: [service].project_id is required")
	}
	if engineStr != "liquid" {
		return fmt.Errorf("only 'liquid' engine is supported in this tracer bullet")
	}

	fmt.Printf("=> Deploying %s (Engine: Liquid)...\n", name)

	// 2. Build the artifact
	//
	// If liquid-metal.toml has a [build] section, run the user-supplied command.
	// Otherwise default to compiling Go → WebAssembly (GOOS=wasip1).
	//
	// liquid-metal.toml examples:
	//   Go (default — no [build] section needed):
	//     auto: GOOS=wasip1 GOARCH=wasm go build -o main.wasm .
	//
	//   Rust:
	//     [build]
	//     command = "cargo build --target wasm32-wasip1 --release"
	//     output  = "target/wasm32-wasip1/release/my_fn.wasm"
	//
	//   Any language:
	//     [build]
	//     command = "make wasm"
	//     output  = "dist/handler.wasm"
	buildCommand := cfg.GetString("build.command")
	wasmFile := cfg.GetString("build.output")
	if wasmFile == "" {
		wasmFile = "main.wasm"
	}

	if buildCommand != "" {
		fmt.Printf("=> Building (%s)...\n", buildCommand)
		sh := exec.Command("sh", "-c", buildCommand)
		sh.Stdout = os.Stdout
		sh.Stderr = os.Stderr
		if err := sh.Run(); err != nil {
			return fmt.Errorf("build failed: %w", err)
		}
	} else {
		fmt.Println("=> Compiling Go to WebAssembly (wasip1)...")
		goCmd := exec.Command("go", "build", "-o", wasmFile, ".")
		goCmd.Env = append(os.Environ(), "GOOS=wasip1", "GOARCH=wasm")
		goCmd.Stdout = os.Stdout
		goCmd.Stderr = os.Stderr
		if err := goCmd.Run(); err != nil {
			return fmt.Errorf("compilation failed: %w", err)
		}
	}
	defer os.Remove(wasmFile)

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

	// 4. Request pre-signed upload URL
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

	// 5. Upload the artifact
	fmt.Println("=> Uploading artifact to object storage...")
	httpReq, err := http.NewRequest("PUT", uploadUrl, bytes.NewReader(fileBytes))
	if err != nil {
		return fmt.Errorf("create upload request: %w", err)
	}
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

	// 6. Confirm deployment
	fmt.Println("=> Finalizing deployment...")
	deployReq := withToken(connect.NewRequest(&v1.DeployRequest{
		Name:        name,
		Slug:        name,
		Engine:      v1.Engine_ENGINE_LIQUID,
		ProjectId:   projectID,
		DeployId:    deployID,
		ArtifactKey: artifactKey,
		Sha256:      sha256Hex,
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

