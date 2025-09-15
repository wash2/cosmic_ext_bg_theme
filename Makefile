BIN_DIR := $(HOME)/.local/bin
DESKTOP_NAME := cosmic.ext.BgTheme.desktop
BINARY_FILE := cosmic-ext-bg-theme
BINARY_PATH := $(BIN_DIR)/$(BINARY_FILE)
DESKTOP_FILE := $(HOME)/.local/share/applications
DESKTOP_PATH := $(DESKTOP_FILE)/$(DESKTOP_NAME)
TARGET = debug
DEBUG ?= 0

VENDOR ?= 0
ifneq ($(VENDOR),0)
	ARGS += --offline --locked
endif

ifeq ($(DEBUG),0)
	TARGET = release
	ARGS += --release
endif

all: extract-vendor
	cargo build $(ARGS)

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
	install -D -m 644 "res/$(DESKTOP_NAME)" "$(DESKTOP_PATH)"

	@echo "Cosmic Background Theme installed"

uninstall:

	@echo "Removing executable..."
	rm -f $(BINARY_PATH)

	@echo "Cosmic Background Theme uninstalled successfully!"

.PHONY = all clean install uninstall vendor

