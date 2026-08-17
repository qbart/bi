PREFIX ?= $(HOME)/bin

.PHONY: build install

build:
	cargo build --release

install: build
	mkdir -p $(PREFIX)
	cp target/release/bi $(PREFIX)/bi
