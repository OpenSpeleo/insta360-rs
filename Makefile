.PHONY: setup update

CARGO ?= cargo

setup:
	curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
	# $(CARGO) install cargo-binstall --locked
	$(CARGO) binstall --locked --no-confirm cargo-audit
	$(CARGO) binstall --locked --no-confirm cargo-deny
	$(CARGO) binstall --locked --no-confirm cargo-machete
	$(CARGO) binstall --locked --no-confirm cargo-nextest
	$(CARGO) binstall --locked --no-confirm cargo-outdated
	$(CARGO) binstall --locked --no-confirm cargo-wizard
	$(CARGO) binstall --locked --no-confirm kache
	$(CARGO) binstall --locked --no-confirm prek

update:
	$(CARGO) update
	$(CARGO) outdated --workspace --depth 1
