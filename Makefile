# genie-core — Build, Test, Cross-Compile, Deploy
#
# Usage:
#   make                       Build debug binaries (x86_64)
#   make test                  Run all tests
#   make soak-selfcheck        Validate the issue #113 soak analyzer (no hardware)
#   make release               Build optimized x86_64 binaries
#   make jetson                Cross-compile release for aarch64 (Jetson)
#   make deploy                Deploy to Jetson devkit via SSH
#   make jetson-ai-runtime     Build + install genie-ai-runtime v1.0.0 on the
#                              Jetson alongside llama.cpp (issue #54). Install-
#                              only — does not flip the backend config or stop
#                              genie-llm.service.
#   make clean                 Remove build artifacts
#
# Configuration:
#   JETSON_HOST       SSH target (default: geniepod.local)
#   JETSON_USER       SSH user (default: geniepod)

JETSON_HOST ?= geniepod.local
JETSON_USER ?= geniepod
PYTHON ?= python3
JETSON_TARGET = $(JETSON_USER)@$(JETSON_HOST)
AARCH64 = aarch64-unknown-linux-gnu
GENIE_CORE_FEATURES ?=

GENIE_CORE_FEATURE_ARGS = $(if $(strip $(GENIE_CORE_FEATURES)),--features $(GENIE_CORE_FEATURES),)

BINARIES = genie-core genie-ctl genie-governor genie-health genie-api
RELEASE_DIR = target/release
CROSS_DIR = target/$(AARCH64)/release
INSTALL_DIR = /opt/geniepod
# Every helper under deploy/scripts/ deploys to $(INSTALL_DIR)/bin on the Jetson.
# A wildcard means a newly added script ships automatically (no list to update).
JETSON_SCRIPTS = $(wildcard deploy/scripts/*)

.PHONY: all build test release jetson deploy deploy-config deploy-systemd clean check fmt jetson-ai-runtime soak-selfcheck

# ── Development ─────────────────────────────────────────────────

all: build

build:
	cargo build

check:
	cargo check

test:
	cargo test

# Score the committed example fixture and assert the soak analyzer's verdicts
# (issue #113). Hardware-free — exercises tests/soak/analyze_soak.py end to end.
soak-selfcheck:
	$(PYTHON) tests/soak/analyze_soak.py --self-check

fmt:
	cargo fmt --all

# ── Release ─────────────────────────────────────────────────────

release:
	cargo build --release -p genie-core $(GENIE_CORE_FEATURE_ARGS)
	cargo build --release -p genie-ctl -p genie-governor -p genie-health -p genie-api
	@echo "Release binaries:"
	@ls -lh $(foreach bin,$(BINARIES),$(RELEASE_DIR)/$(bin))

# ── Cross-compile for Jetson (aarch64) ──────────────────────────

jetson: jetson-prereqs
	CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
	AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
	cargo build --release --target $(AARCH64) -p genie-core $(GENIE_CORE_FEATURE_ARGS)
	CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
	AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
	cargo build --release --target $(AARCH64) -p genie-ctl -p genie-governor -p genie-health -p genie-api
	@echo "Jetson binaries:"
	@ls -lh $(foreach bin,$(BINARIES),$(CROSS_DIR)/$(bin))

jetson-prereqs:
	@which aarch64-linux-gnu-gcc > /dev/null 2>&1 || \
		(echo "ERROR: aarch64-linux-gnu-gcc not found. Install with:" && \
		 echo "  sudo apt install gcc-aarch64-linux-gnu" && exit 1)
	@rustup target list --installed | grep -q $(AARCH64) || \
		(echo "Adding Rust target $(AARCH64)..." && rustup target add $(AARCH64))

# ── Deploy to Jetson ────────────────────────────────────────────

deploy: jetson deploy-binaries deploy-config deploy-systemd deploy-docker deploy-setup
	@echo ""
	@echo "=== Deployed to $(JETSON_TARGET) ==="
	@echo "  Binaries: $(INSTALL_DIR)/bin/"
	@echo "  Config:   /etc/geniepod/"
	@echo "  Systemd:  /etc/systemd/system/"
	@echo ""
	@echo "Run first-time setup on the Jetson:"
	@echo "  ssh $(JETSON_TARGET) 'bash $(INSTALL_DIR)/setup-jetson.sh'"

deploy-binaries:
	ssh $(JETSON_TARGET) 'sudo mkdir -p $(INSTALL_DIR)/bin'
	$(foreach bin,$(BINARIES), \
		scp $(CROSS_DIR)/$(bin) $(JETSON_TARGET):/tmp/$(bin) && \
		ssh $(JETSON_TARGET) 'sudo mv /tmp/$(bin) $(INSTALL_DIR)/bin/$(bin) && sudo chmod 755 $(INSTALL_DIR)/bin/$(bin)' ; \
	)

deploy-config:
	ssh $(JETSON_TARGET) 'sudo mkdir -p /etc/geniepod $(INSTALL_DIR)/data && \
		sudo chown -R $(JETSON_USER):$(JETSON_USER) $(INSTALL_DIR)/data'
	scp deploy/config/geniepod.toml $(JETSON_TARGET):/tmp/geniepod.toml
	scp deploy/config/mosquitto.conf $(JETSON_TARGET):/tmp/mosquitto.conf
	ssh $(JETSON_TARGET) 'sudo cp /tmp/geniepod.toml /etc/geniepod/geniepod.toml && \
		sudo cp /tmp/mosquitto.conf /etc/geniepod/mosquitto.conf && \
		sudo chmod 600 /etc/geniepod/geniepod.toml /etc/geniepod/mosquitto.conf'
	@echo "Config deployed — /etc/geniepod/geniepod.toml refreshed from repo."
	@echo "WARNING: any hand-edits on the target were overwritten. Keep secrets in env vars"
	@echo "         (HA_TOKEN, TELEGRAM_BOT_TOKEN, etc.), not in geniepod.toml directly."

deploy-systemd:
	scp deploy/systemd/*.service deploy/systemd/*.target $(JETSON_TARGET):/tmp/
	ssh $(JETSON_TARGET) 'for unit in /tmp/genie-*.service /tmp/homeassistant.service /tmp/geniepod*.target; do \
		sudo install -m 0644 "$$unit" "/etc/systemd/system/$$(basename "$$unit")"; \
	done; \
		sudo systemctl daemon-reload'

deploy-docker:
	ssh $(JETSON_TARGET) 'sudo mkdir -p $(INSTALL_DIR)/docker'
	scp deploy/docker/docker-compose.yml $(JETSON_TARGET):/tmp/docker-compose.yml
	ssh $(JETSON_TARGET) 'sudo mv /tmp/docker-compose.yml $(INSTALL_DIR)/docker/docker-compose.yml && \
		sudo chmod 644 $(INSTALL_DIR)/docker/docker-compose.yml'

deploy-setup:
	scp deploy/setup-jetson.sh $(JETSON_TARGET):/tmp/
	ssh $(JETSON_TARGET) 'rm -rf /tmp/geniepod-scripts && mkdir -p /tmp/geniepod-scripts'
	scp $(JETSON_SCRIPTS) $(JETSON_TARGET):/tmp/geniepod-scripts/
	ssh $(JETSON_TARGET) 'sudo cp /tmp/setup-jetson.sh $(INSTALL_DIR)/setup-jetson.sh && sudo chmod +x $(INSTALL_DIR)/setup-jetson.sh && sudo mkdir -p $(INSTALL_DIR)/bin && sudo cp /tmp/geniepod-scripts/* $(INSTALL_DIR)/bin/ && sudo chmod +x $(addprefix $(INSTALL_DIR)/bin/,$(notdir $(JETSON_SCRIPTS))) && rm -rf /tmp/geniepod-scripts'

# ── Alternate LLM runtime: genie-ai-runtime (issue #54) ─────────
#
# Builds genie-ai-runtime v1.0.0 from source on the Jetson and installs the
# binaries alongside the existing llama.cpp setup. The config switch
# ([services.llm].backend / .systemd_unit) is intentionally left for the
# operator to make by hand — this target is install-only, mirroring the
# `--model qwen3-4b` safety property from PR #46.
#
# Prereq: `make deploy` (or at least deploy-systemd + deploy-setup) has run
# at least once so the new units and the updated setup-jetson.sh are on the
# target. We re-run those two sub-targets here for a self-contained flow.

jetson-ai-runtime: deploy-systemd deploy-setup
	@echo ""
	@echo "=== Building + installing genie-ai-runtime v1.0.0 on $(JETSON_TARGET) ==="
	@echo "    (this takes 10-20 min on Orin Nano while CMake compiles CUDA)"
	@echo ""
	ssh $(JETSON_TARGET) 'bash $(INSTALL_DIR)/setup-jetson.sh --runtime genie-ai-runtime'

# ── Docker (HA + opt-in services) ───────────────────────────────

docker-up:
	ssh $(JETSON_TARGET) 'sudo systemctl start homeassistant'

docker-sovereign:
	ssh $(JETSON_TARGET) 'cd /opt/geniepod/docker && sudo docker compose --profile sovereign up -d'

# ── Clean ───────────────────────────────────────────────────────

clean:
	cargo clean
