PREFIX ?= $(HOME)/bin

.PHONY: build install build-gui install-gui

build:
	cargo build --release

install: build
	mkdir -p $(PREFIX)
	cp target/release/bi $(PREFIX)/bi

# The window is its own program and its own target, so a terminal-only
# install never builds gpui or needs a GPU stack to link. `bi gui` looks for
# bi-gui beside bi, which is where this puts it. See docs/specs/gui.md.
build-gui:
	cargo build --release -p bi-gui

install-gui: build-gui
	mkdir -p $(PREFIX)
	cp target/release/bi-gui $(PREFIX)/bi-gui
