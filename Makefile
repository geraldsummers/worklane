.PHONY: test coverage

test:
	scripts/check-local --quick

coverage:
	scripts/check-local --full
