.PHONY: start stop status test log clean help

help:
	@echo "MultiGraph Server Management"
	@echo "Usage: make [target]"
	@echo ""
	@echo "Targets:"
	@echo "  start   Start the server in background (release mode)"
	@echo "  stop    Stop the running server"
	@echo "  status  Check server process status"
	@echo "  test    Run full lifecycle integration tests"
	@echo "  bench   Run the server benchmark"
	@echo "  log     Tail the server output logs"
	@echo "  clean   Remove logs, pid file, and data directory"

start:
	@bash scripts/start_server.sh

stop:
	@bash scripts/stop_server.sh

status:
	@bash scripts/status_server.sh

test:
	@bash scripts/test_scripts.sh

bench:
	@bash scripts/bench_server.sh

log:
	@tail -f server.log

clean:
	@rm -f server.pid server.log
	@rm -rf data/