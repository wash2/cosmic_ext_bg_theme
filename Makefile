BIN_DIR := $(HOME)/.local/bin
SERVICE_DIR := $(HOME)/.config/systemd/user
SERVICE_NAME := cosmic-bg-theme.service
BINARY_FILE := cosmic-bg-theme
BINARY_PATH := $(BIN_DIR)/$(BINARY_FILE)
SERVICE_FILE := $(SERVICE_DIR)/$(SERVICE_NAME)
TARGET = debug
DEBUG ?= 0
TOOLCHAIN ?= stable

VENDOR ?= 0
ifneq ($(VENDOR),0)
	ARGS += --offline --locked
endif

ifeq ($(DEBUG),0)
	TARGET = release
	ARGS += --release
endif

all: extract-vendor
	cargo +$(TOOLCHAIN) build $(ARGS)

clean:
	cargo clean

distclean:
	rm -rf .cargo vendor vendor.tar target

vendor:
	mkdir -p .cargo
	cargo +$(TOOLCHAIN) vendor | head -n -1 > .cargo/config
	echo 'directory = "vendor"' >> .cargo/config
	tar pcf vendor.tar vendor
	rm -rf vendor

extract-vendor:
ifeq ($(VENDOR),1)
	rm -rf vendor; tar pxf vendor.tar
endif

install:
	@echo "Installing executable to $(BIN_DIR)..."
	install -D -m 755 "target/$(TARGET)/$(BINARY_FILE)" "$(BINARY_PATH)"

	@echo "Installing systemd service file to $(SERVICE_DIR)..."
	install -D -m 644 $(SERVICE_NAME) $(SERVICE_FILE)

	@echo "Updating ExecStart line in the service file..."
	sed -i "s|ExecStart=.*|ExecStart=$(BINARY_PATH)|g" $(SERVICE_FILE)
	
	@echo "Reloading systemd user units..."
	if [ "$(SERVICE_DIR)" = "$(HOME)/.config/systemd/user" ]; then \
		echo "Updating graphical.target line in the service file..."; \
		sed -i 's/graphical.target/graphical-session.target/g' $(SERVICE_FILE); \
		systemctl --user daemon-reload; \
	else \
		echo "Requesting sudo access to reload systemd user units..."; \
		systemctl daemon-reload; \
	fi

	@echo "Enabling and starting the service..."
	if [ "$(SERVICE_DIR)" = "$(HOME)/.config/systemd/user" ]; then \
		systemctl --user enable --now $(SERVICE_NAME); \
	else \
		echo "Requesting sudo access to enable and start the service..."; \
		systemctl enable --now $(SERVICE_FILE); \
	fi

	@echo "Cosmic Background Theme installed and service started successfully!"

uninstall:
	@echo "Disabling and stopping the service..."
	if [ "$(SERVICE_DIR)" = "$(HOME)/.config/systemd/user" ]; then \
		systemctl --user disable --now $(SERVICE_NAME); \
	else \
		echo "Requesting sudo access to disable and stop the service..."; \
		systemctl disable --now $(SERVICE_FILE); \
	fi

	@echo "Removing executable and systemd service file..."
	rm -f $(BINARY_PATH) $(SERVICE_FILE)

	@echo "Reloading systemd user units..."
	if [ "$(SERVICE_DIR)" = "$(HOME)/.config/systemd/user" ]; then \
		systemctl --user daemon-reload; \
	else \
		echo "Requesting sudo access to reload systemd user units..."; \
		systemctl daemon-reload; \
	fi

	@echo "Cosmic Background Theme uninstalled successfully!"

.PHONY = all clean install uninstall vendor
