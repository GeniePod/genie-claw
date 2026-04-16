# genie-core — Build, Test, Cross-Compile, Deploy
#
# Usage:
#   make              Build debug binaries (x86_64)
#   make test         Run all tests
#   make release      Build optimized x86_64 binaries
#   make jetson       Cross-compile release for aarch64 (Jetson)
#   make deploy       Deploy to Jetson devkit via SSH
#   make clean        Remove build artifacts
#
# Configuration:
#   JETSON_HOST       SSH target (default: geniepod.local)
#   JETSON_USER       SSH user (default: geniepod)

JETSON_HOST ?= geniepod.local
JETSON_USER ?= geniepod
JETSON_TARGET = $(JETSON_USER)@$(JETSON_HOST)
AARCH64 = aarch64-unknown-linux-gnu

BINARIES = genie-core genie-ctl genie-governor genie-health genie-api
RELEASE_DIR = target/release
CROSS_DIR = target/$(AARCH64)/release
INSTALL_DIR = /opt/geniepod

.PHONY: all build test release jetson deploy deploy-config deploy-systemd clean check fmt

# ── Development ─────────────────────────────────────────────────

all: build

build:
	cargo build

check:
	cargo check

test:
	cargo test

fmt:
	cargo fmt --all

# ── Release ─────────────────────────────────────────────────────

release:
	cargo build --release
	@echo "Release binaries:"
	@ls -lh $(foreach bin,$(BINARIES),$(RELEASE_DIR)/$(bin))

# ── Cross-compile for Jetson (aarch64) ──────────────────────────

jetson: jetson-prereqs
	CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
	AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
	cargo build --release --target $(AARCH64)
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
	ssh $(JETSON_TARGET) 'sudo cp -n /tmp/geniepod.toml /etc/geniepod/ && \
		sudo cp -n /tmp/mosquitto.conf /etc/geniepod/ && \
		sudo chmod 600 /etc/geniepod/geniepod.toml /etc/geniepod/mosquitto.conf'
	@echo "Config deployed (existing files NOT overwritten — using cp -n)"

deploy-systemd:
	scp deploy/systemd/*.service deploy/systemd/*.target $(JETSON_TARGET):/tmp/
	ssh $(JETSON_TARGET) 'sudo cp /tmp/genie-*.service /tmp/homeassistant.service /tmp/geniepod*.target /etc/systemd/system/ 2>/dev/null; \
		sudo systemctl daemon-reload'

deploy-docker:
	ssh $(JETSON_TARGET) 'sudo mkdir -p $(INSTALL_DIR)/docker'
	scp deploy/docker/docker-compose.yml $(JETSON_TARGET):/tmp/docker-compose.yml
	ssh $(JETSON_TARGET) 'sudo mv /tmp/docker-compose.yml $(INSTALL_DIR)/docker/docker-compose.yml && \
		sudo chmod 644 $(INSTALL_DIR)/docker/docker-compose.yml'

deploy-setup:
	scp deploy/setup-jetson.sh $(JETSON_TARGET):/tmp/
	scp deploy/scripts/genie-wake-listen.py deploy/scripts/genie-wakeword.py deploy/scripts/detect-audio-device.sh $(JETSON_TARGET):/tmp/
	ssh $(JETSON_TARGET) 'sudo cp /tmp/setup-jetson.sh $(INSTALL_DIR)/setup-jetson.sh && \
		sudo chmod +x $(INSTALL_DIR)/setup-jetson.sh && \
		sudo mkdir -p $(INSTALL_DIR)/bin && \
		sudo cp /tmp/genie-wake-listen.py /tmp/genie-wakeword.py /tmp/detect-audio-device.sh $(INSTALL_DIR)/bin/ && \
		sudo chmod +x $(INSTALL_DIR)/bin/genie-wake-listen.py $(INSTALL_DIR)/bin/genie-wakeword.py $(INSTALL_DIR)/bin/detect-audio-device.sh'

# ── Docker (HA + opt-in services) ───────────────────────────────

docker-up:
	ssh $(JETSON_TARGET) 'sudo systemctl start homeassistant'

docker-sovereign:
	ssh $(JETSON_TARGET) 'cd /opt/geniepod/docker && sudo docker compose --profile sovereign up -d'

# ── Clean ───────────────────────────────────────────────────────

clean:
	cargo clean
