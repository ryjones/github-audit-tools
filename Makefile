# Build the six binaries. The scripts need no build -- they are zsh and
# python -- so `make check` syntax-checks them instead.
#
#   make            debug binaries, into target/debug
#   make release    optimized (lto, stripped), into target/release
#   make test       the unit tests; no network, no credentials
#   make check      fmt, clippy, the tests, and a syntax pass over the scripts
#   make clean      remove target/
#
# CARGO and CARGOFLAGS are overridable: make release CARGOFLAGS=--locked

CARGO     ?= cargo
CARGOFLAGS ?=

BINS    := gh-enterprise-repos gh-org-members gh-org-reports gh-actions-storage \
           clowarden-drift tac-eligibility
ZSH     := $(wildcard *.zsh)
PYTHON  := $(wildcard *.py)

.DEFAULT_GOAL := debug
.PHONY: debug release all test check fmt clippy scripts clean help

debug:
	$(CARGO) build $(CARGOFLAGS)
	@printf 'target/debug/%s\n' $(BINS)

release:
	$(CARGO) build --release $(CARGOFLAGS)
	@printf 'target/release/%s\n' $(BINS)

all: debug

test:
	$(CARGO) test $(CARGOFLAGS)

# Everything that can be checked without a GitHub credential.
check: fmt clippy test scripts

fmt:
	$(CARGO) fmt --check

clippy:
	$(CARGO) clippy --all-targets $(CARGOFLAGS)

# One line per script, so a failure says which file it was.
scripts:
	@for f in $(ZSH); do zsh -n $$f && echo "  ok  $$f"; done
	@for f in $(PYTHON); do \
		python3 -c 'import ast,sys; ast.parse(open(sys.argv[1]).read())' $$f \
		&& echo "  ok  $$f"; \
	done

clean:
	$(CARGO) clean

help:
	@sed -n '3,9p' $(firstword $(MAKEFILE_LIST)) | sed 's/^# \{0,1\}//'
