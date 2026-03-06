# Installation

## Requirements

| Requirement | Notes |
|-------------|-------|
| Go 1.21+ or Rust 1.75+ | Only if building locally |
| `plat` CLI | Required for deploys |
| Git | Required for push-triggered deploys |

## Install the CLI

**macOS / Linux:**

```bash
curl -fsSL https://plat.run/install.sh | sh
```

**Homebrew:**

```bash
brew install project-platform/tap/plat
```

**From source:**

```bash
go install github.com/kendricklawton/project-platform/plat@latest
```

## Authenticate

```bash
plat auth login      # opens browser
plat auth status     # confirm login
```

Your token is stored in the system keychain — never in a plain file.

## Verify

```bash
plat version
# plat v0.1.0-alpha (go1.25 darwin/arm64)
```

## Uninstall

```bash
plat auth logout
rm $(which plat)
```
