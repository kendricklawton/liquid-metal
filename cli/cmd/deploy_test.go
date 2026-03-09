package cmd

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
	"github.com/spf13/viper"
)

func writeTempMachineToml(t *testing.T, content string) func() {
	t.Helper()
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "liquid-metal.toml"), []byte(content), 0644); err != nil {
		t.Fatalf("write liquid-metal.toml: %v", err)
	}
	orig, _ := os.Getwd()
	if err := os.Chdir(dir); err != nil {
		t.Fatalf("chdir: %v", err)
	}
	return func() { os.Chdir(orig) }
}

func parseMachineToml(t *testing.T) (name, engineStr string, engine v1.Engine, port, vcpu, memoryMB int32, err error) {
	t.Helper()
	cfg := viper.New()
	cfg.SetConfigName("liquid-metal")
	cfg.SetConfigType("toml")
	cfg.AddConfigPath(".")
	if readErr := cfg.ReadInConfig(); readErr != nil {
		err = readErr
		return
	}
	name = cfg.GetString("service.name")
	engineStr = strings.ToLower(cfg.GetString("service.engine"))
	port = cfg.GetInt32("service.port")
	vcpu = cfg.GetInt32("metal.vcpu")
	memoryMB = cfg.GetInt32("metal.memory_mb")

	switch engineStr {
	case "metal":
		engine = v1.Engine_ENGINE_METAL
	case "liquid":
		engine = v1.Engine_ENGINE_LIQUID
	default:
		engine = v1.Engine_ENGINE_UNSPECIFIED
	}
	return
}

func TestDeployParsesMachineTomlMetal(t *testing.T) {
	restore := writeTempMachineToml(t, `
[service]
name   = "my-app"
engine = "metal"
port   = 8080

[metal]
vcpu      = 2
memory_mb = 256
`)
	defer restore()

	name, engineStr, engine, port, vcpu, memMB, err := parseMachineToml(t)
	if err != nil {
		t.Fatalf("parseMachineToml: %v", err)
	}
	if name != "my-app" {
		t.Errorf("name = %q, want %q", name, "my-app")
	}
	if engineStr != "metal" {
		t.Errorf("engine string = %q, want %q", engineStr, "metal")
	}
	if engine != v1.Engine_ENGINE_METAL {
		t.Errorf("engine = %v, want ENGINE_METAL", engine)
	}
	if port != 8080 {
		t.Errorf("port = %d, want 8080", port)
	}
	if vcpu != 2 {
		t.Errorf("vcpu = %d, want 2", vcpu)
	}
	if memMB != 256 {
		t.Errorf("memory_mb = %d, want 256", memMB)
	}
}

func TestDeployParsesMachineTomlLiquid(t *testing.T) {
	restore := writeTempMachineToml(t, `
[service]
name   = "my-fn"
engine = "liquid"
`)
	defer restore()

	name, _, engine, _, _, _, err := parseMachineToml(t)
	if err != nil {
		t.Fatalf("parseMachineToml: %v", err)
	}
	if name != "my-fn" {
		t.Errorf("name = %q, want %q", name, "my-fn")
	}
	if engine != v1.Engine_ENGINE_LIQUID {
		t.Errorf("engine = %v, want ENGINE_LIQUID", engine)
	}
}

func TestDeployFailsWithoutMachineToml(t *testing.T) {
	dir := t.TempDir()
	orig, _ := os.Getwd()
	os.Chdir(dir)
	defer os.Chdir(orig)

	cfg := viper.New()
	cfg.SetConfigName("liquid-metal")
	cfg.SetConfigType("toml")
	cfg.AddConfigPath(".")
	if err := cfg.ReadInConfig(); err == nil {
		t.Error("expected error reading missing liquid-metal.toml, got nil")
	}
}
