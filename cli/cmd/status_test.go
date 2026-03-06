package cmd

import (
	"testing"

	v1 "github.com/kendricklawton/liquid-metal/gen/go/liquidmetal/v1"
)

func TestEngineLabel(t *testing.T) {
	tests := []struct {
		engine v1.Engine
		want   string
	}{
		{v1.Engine_ENGINE_METAL, "metal"},
		{v1.Engine_ENGINE_LIQUID, "liquid"},
		{v1.Engine_ENGINE_UNSPECIFIED, "unknown"},
	}
	for _, tc := range tests {
		if got := engineLabel(tc.engine); got != tc.want {
			t.Errorf("engineLabel(%v) = %q, want %q", tc.engine, got, tc.want)
		}
	}
}

func TestStatusLabel(t *testing.T) {
	tests := []struct {
		status v1.ServiceStatus
		want   string
	}{
		{v1.ServiceStatus_SERVICE_STATUS_RUNNING, "running"},
		{v1.ServiceStatus_SERVICE_STATUS_PROVISIONING, "provisioning"},
		{v1.ServiceStatus_SERVICE_STATUS_FAILED, "failed"},
		{v1.ServiceStatus_SERVICE_STATUS_STOPPED, "stopped"},
		{v1.ServiceStatus_SERVICE_STATUS_UNSPECIFIED, "unknown"},
	}
	for _, tc := range tests {
		if got := statusLabel(tc.status); got != tc.want {
			t.Errorf("statusLabel(%v) = %q, want %q", tc.status, got, tc.want)
		}
	}
}
