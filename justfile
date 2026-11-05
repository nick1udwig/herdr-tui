fmt:
    cargo fmt --all

lint:
    cargo fmt --all -- --check
    cargo clippy --all-targets --locked -- -D warnings

test:
    cargo test --all-targets --locked

check: lint test

build:
    cargo build --locked

integration: build
    python3 tests/integration_pty.py
    python3 tests/colors.py
    python3 tests/pane_exit.py
    python3 tests/group_close.py
    python3 tests/git_polling.py
    python3 tests/pane_move.py
    python3 tests/worktree_create.py
    python3 tests/worktree_remove.py
    python3 tests/attention.py
    python3 tests/startup.py
    python3 tests/new_directory.py
    python3 tests/keys.py
    python3 tests/settings.py
    python3 tests/herdr_config.py
    python3 tests/ui_options.py
    python3 tests/notifications.py
    python3 tests/mobile.py
    python3 tests/selection.py

workflows-integration:
    cargo test workspace_and_worktree_workflows -- --ignored

# Pass display=/run/user/$UID/wayland-1 to draw on an existing compositor instead of Weston.
app-integration display="": build
    python3 scripts/app_integration.py {{ if display != "" { "--display " + quote(display) } else { "" } }}

package output:
    python3 scripts/package.py {{quote(output)}}

release:
    cargo build --release --locked

install prefix: release
    install -Dm755 target/release/herder-tui {{quote(prefix)}}/bin/herder-tui

cli-integration: build
    python3 tests/cli.py

discovery-integration: build
    python3 tests/discovery.py

chrome-integration: build
    python3 tests/chrome.py

resize-integration: build
    python3 tests/resize.py

msrv:
    cargo +1.89.0 check --all-targets --locked

notices:
    python3 scripts/notices.py

remote-integration: release
    python3 tests/remote.py

geometry-integration: release
    python3 tests/geometry.py

profile: release
    python3 tests/performance.py

compatibility-integration: release
    python3 tests/compatibility.py

package-check:
    python3 scripts/check-package.py

cross-check:
    cargo clippy --target x86_64-pc-windows-gnu --all-targets --locked -- -D warnings
    cargo clippy --target aarch64-apple-darwin --all-targets --locked -- -D warnings
