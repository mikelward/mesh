# Shortcuts for the commands DEVELOPMENT.md spells out in full, so the common
# ones are `make install` / `make check` rather than a flag string to recall.
#
# Every target is a thin wrapper over a single cargo invocation: cargo stays the
# build system, and this file stays a table of entry points rather than a second
# one. `makefile_test.sh` checks that the wrappers still say what CI says.

CARGO ?= cargo

# The workspace root is a virtual manifest, so anything package-scoped -- most
# of all `cargo install` -- has to name the package directory.
PACKAGE := crates/mesh

.DEFAULT_GOAL := build
.PHONY: build release run install uninstall test fmt lint check clean help

build: ## Debug build of the whole workspace (the default target)
	$(CARGO) build --workspace

release: ## Optimized build → target/release/mesh
	$(CARGO) build --workspace --release

run: ## Build and start the shell
	$(CARGO) run -p mesh

# --locked installs the exact versions from the committed Cargo.lock. Set
# CARGO_INSTALL_ROOT to land the binary somewhere other than ~/.cargo/bin.
install: ## Install the mesh binary into ~/.cargo/bin
	$(CARGO) install --locked --path $(PACKAGE)

uninstall: ## Remove an installed mesh binary
	$(CARGO) uninstall mesh

test: ## Run every suite: the cargo tests and the shell-script ones
	$(CARGO) test --workspace
	sh session_start_hook_test.sh
	sh makefile_test.sh

fmt: ## Reformat the tree
	$(CARGO) fmt --all

lint: ## The formatting and clippy gates, exactly as CI runs them
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --all-targets --all-features -- -D warnings

check: lint test ## Everything CI checks, before pushing

clean: ## Delete the target directory
	$(CARGO) clean

help: ## List these targets
	@awk -F':.*## ' '/^[a-z][a-z-]*:.*## /{printf "  %-10s %s\n", $$1, $$2}' $(MAKEFILE_LIST)
