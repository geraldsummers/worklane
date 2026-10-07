FROM debian:trixie-slim
# worklane-standard-containerfile
ARG CODEX_VERSION=0.157.0
ARG NODE_VERSION=24.21.0
ARG NODE_SHA256=fd8e59d5a511510f6a298afb548f18c7d2b1be404d8b4a27d94fbe49f56cb2d6
ARG PI_VERSION=1.0.2
ARG PI_WEB_VERSION=0.35.0
ARG PI_PLAN_VERSION=0.58.4
COPY assets/pi-bundle/package.json assets/pi-bundle/package-lock.json /opt/worklane/pi-seed/
ARG TYPESCRIPT_VERSION=7.0.2
ARG PRETTIER_VERSION=3.9.5
ARG RUST_VERSION=1.85.1
ARG RUSTUP_INIT_SHA256=7d0ea0f8eba7fa1ebfe998091cd7ec4501e33ec5ca6b884eb4d894d7da5170af
ARG HERDR_VERSION=0.8.2
ARG HERDR_SHA256=976150a14d490c94b243ea2e1a7eb2dfb67f12e36b182db90936f6728e6aecf4
ARG NVIM_VERSION=0.12.5
ARG NVIM_SHA256=bce0f56eda1f1b1db6eee8f4133d7a38813ea07933837dd1777411ca384c6875
ARG TREE_SITTER_VERSION=0.26.1
ARG TREE_SITTER_SHA256=d74182fffbf441f247371ad6af77d99b10ac9f788d7e068e4b44992d9d6d7c26
ARG PI_LIGHTPANDA_VERSION=0.1.0
ARG LIGHTPANDA_VERSION=1.0.0
ARG LIGHTPANDA_SHA256=aa5a4b8ed53d1e38b3c73f5b2647d0a84a82e6744557f45f9a9c85858aa031c3
ARG USERNAME=dev
ARG USER_UID=1001
ARG USER_GID=1001
ENV DEBIAN_FRONTEND=noninteractive SHELL=/usr/bin/zsh LANG=C.UTF-8 LC_ALL=C.UTF-8 \
    RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/home/${USERNAME}/.cargo \
    CARGO_INSTALL_ROOT=/home/${USERNAME}/.local \
    GOBIN=/home/${USERNAME}/.local/bin NPM_CONFIG_PREFIX=/home/${USERNAME}/.local \
    PATH=/home/${USERNAME}/.local/bin:/home/${USERNAME}/.cargo/bin:/usr/local/cargo/bin:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends \
    bash-completion bat bc bison build-essential ca-certificates clang cmake curl \
    dbus-user-session dbus-x11 diffutils dnsutils fd-find file findutils fonts-liberation \
    fonts-noto-color-emoji fzf gawk gdb git git-lfs gnupg golang-go jq less \
    libffi-dev libsqlite3-dev libssl-dev lsof make man-db nano ncdu \
    netcat-openbsd ninja-build openssh-client patch \
    pkg-config procps psmisc python-is-python3 python3 python3-pip python3-venv systemd systemd-sysv \
    graphicsmagick pipx python3-tomlkit ripgrep rsync shellcheck socat sqlite3 strace tar tree unzip \
    tzdata valgrind vim wget xvfb xauth xz-utils yq zip zsh chromium \
 && rm -rf /var/lib/apt/lists/* \
 && ln -s /usr/bin/batcat /usr/local/bin/bat \
 && ln -s /usr/bin/fdfind /usr/local/bin/fd \
 && curl --proto '=https' --tlsv1.2 -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz" -o /tmp/node.tar.xz \
 && echo "${NODE_SHA256}  /tmp/node.tar.xz" | sha256sum -c - \
 && tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 \
 && rm -f /tmp/node.tar.xz \
 && codex_version="$(npm view "@openai/codex@${CODEX_VERSION}" version --fetch-retries=2)" \
 && test -n "$codex_version" \
 && env NPM_CONFIG_PREFIX=/usr/local npm install -g "@openai/codex@${codex_version}" "typescript@${TYPESCRIPT_VERSION}" "prettier@${PRETTIER_VERSION}" \
 && npm ci --prefix /opt/worklane/pi-seed --omit=dev --ignore-scripts --no-audit --no-fund \
 && ln -s /opt/worklane/pi-seed/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js /usr/local/bin/pi \
 && install -d /etc/worklane \
 && printf '%s\n' "$codex_version" > /etc/worklane/codex-version \
 && printf '%s\n' "$PI_VERSION" > /etc/worklane/pi-version \
 && printf '%s\n' "$PI_WEB_VERSION" > /etc/worklane/pi-web-version \
 && printf '%s\n' "$PI_PLAN_VERSION" > /etc/worklane/pi-plan-version \
 && ln -s /opt/worklane/pi-seed/node_modules/pi-web-access /opt/worklane/pi-web-access \
 && test "$(pi --version)" = "$PI_VERSION" \
 && test "$(node -p 'require("/opt/worklane/pi-seed/node_modules/pi-web-access/package.json").version')" = "$PI_WEB_VERSION" \
 && test "$(node -p 'require("/opt/worklane/pi-seed/node_modules/@narumitw/pi-plan-mode/package.json").version')" = "$PI_PLAN_VERSION" \
 && test "$(node -p 'require("/opt/worklane/pi-seed/node_modules/pi-lightpanda-extension/package.json").version')" = "$PI_LIGHTPANDA_VERSION" \
 && curl -fsSL "https://github.com/lightpanda-io/browser/releases/download/${LIGHTPANDA_VERSION}/lightpanda-x86_64-linux" -o /tmp/lightpanda \
 && echo "${LIGHTPANDA_SHA256}  /tmp/lightpanda" | sha256sum -c - \
 && install -m 755 /tmp/lightpanda /usr/local/bin/lightpanda \
 && rm -f /tmp/lightpanda \
 && printf '%s\n' "$LIGHTPANDA_VERSION" > /etc/worklane/lightpanda-version \
 && test "$(lightpanda version)" = "$LIGHTPANDA_VERSION" \
 && mv /usr/local/bin/codex /usr/local/bin/codex-real \
 && printf '%s\n' \
    '#!/bin/sh' \
    'host_auth=/run/worklane-host-codex/auth.json' \
    'if [ -f "$host_auth" ]; then' \
    '  case "${1:-}" in' \
    '    login|logout) echo "Codex credentials are managed on the host; sign in or out there" >&2; exit 2 ;;' \
    '  esac' \
    '  lane_auth="$HOME/.codex/auth.json"' \
    '  if [ ! -e "$lane_auth" ] && [ ! -L "$lane_auth" ]; then ln -s "$host_auth" "$lane_auth"; fi' \
    'fi' \
    'for arg do' \
    '  case "$arg" in' \
    '    --yolo|--dangerously-bypass-approvals-and-sandbox) exec /usr/local/bin/codex-real "$@" ;;' \
    '  esac' \
    'done' \
    'exec /usr/local/bin/codex-real -a never -s danger-full-access "$@"' \
    > /usr/local/bin/codex \
 && chmod +x /usr/local/bin/codex \
 && mkdir -p /etc/codex \
 && printf '%s\n' 'approval_policy = "never"' 'sandbox_mode = "danger-full-access"' 'check_for_update_on_startup = false' > /etc/codex/config.toml \
 && curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o /tmp/rustup-init.sh \
 && echo "${RUSTUP_INIT_SHA256}  /tmp/rustup-init.sh" | sha256sum -c - \
 && env CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup sh /tmp/rustup-init.sh -y --profile minimal \
 && rm -f /tmp/rustup-init.sh \
 && env CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup rustup toolchain install "${RUST_VERSION}" \
 && env CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup rustup component add rustfmt --toolchain "${RUST_VERSION}" \
 && env CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup rustup default "${RUST_VERSION}" \
 && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
 && echo "deb [signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
 && apt-get update && apt-get install -y --no-install-recommends gh && rm -rf /var/lib/apt/lists/* \
 && curl -fsSL "https://github.com/ogulcancelik/herdr/releases/download/v${HERDR_VERSION}/herdr-linux-x86_64" -o /tmp/herdr \
 && echo "${HERDR_SHA256}  /tmp/herdr" | sha256sum -c - \
 && install -m 755 /tmp/herdr /usr/local/bin/herdr \
 && command -v herdr \
 && herdr --version \
 && rm -f /tmp/herdr \
 && curl -fsSL "https://github.com/neovim/neovim/releases/download/v${NVIM_VERSION}/nvim-linux-x86_64.tar.gz" -o /tmp/nvim.tar.gz \
 && echo "${NVIM_SHA256}  /tmp/nvim.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/nvim.tar.gz -C /opt \
 && ln -s /opt/nvim-linux-x86_64/bin/nvim /usr/local/bin/nvim \
 && rm -f /tmp/nvim.tar.gz \
 && curl -fsSL "https://github.com/tree-sitter/tree-sitter/releases/download/v${TREE_SITTER_VERSION}/tree-sitter-linux-x64.gz" -o /tmp/tree-sitter.gz \
 && echo "${TREE_SITTER_SHA256}  /tmp/tree-sitter.gz" | sha256sum -c - \
 && gzip -dc /tmp/tree-sitter.gz > /usr/local/bin/tree-sitter \
 && chmod 755 /usr/local/bin/tree-sitter \
 && rm -f /tmp/tree-sitter.gz \
 && nvim --version | head -n 1 \
 && tree-sitter --version \
 && python3 --version && python3 -c 'import tomlkit' && gm version && pip3 --version && node --version && npm --version \
 && tsc --version && prettier --version && rustc --version && cargo --version && rustfmt --version
RUN groupadd --gid "${USER_GID}" "${USERNAME}" && useradd --uid "${USER_UID}" --gid "${USER_GID}" -m -s /usr/bin/zsh "${USERNAME}" \
 && truncate -s 0 /etc/machine-id \
 && ln -sfn /etc/machine-id /var/lib/dbus/machine-id \
 && ln -sfn /dev/null /etc/systemd/system/systemd-logind.service \
 && install -d /etc/systemd/system/user@.service.d /etc/systemd/system/user-runtime-dir@.service.d /etc/systemd/system/user-.slice.d /etc/systemd/user.conf.d /usr/lib/systemd/user/default.target.wants \
 && printf '%s\n' \
    '[Unit]' \
    'Description=Worklane container runtime' \
    'Requires=basic.target' \
    "Wants=systemd-user-sessions.service user-runtime-dir@${USER_UID}.service user@${USER_UID}.service" \
    "After=basic.target systemd-user-sessions.service user-runtime-dir@${USER_UID}.service user@${USER_UID}.service" \
    'AllowIsolate=yes' \
    > /usr/lib/systemd/system/worklane.target \
 && ln -sfn /usr/lib/systemd/system/worklane.target /etc/systemd/system/default.target \
 && printf '%s\n' \
    '[Service]' \
    'Environment=XDG_RUNTIME_DIR=/run/user/%i' \
    'Environment=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/%i/bus' \
    'PassEnvironment=WORKLANE_NAME WORKLANE_SESSION WORKLANE_WORKSPACE' \
    > /etc/systemd/system/user@.service.d/worklane.conf \
 && printf '%s\n' \
    '[Slice]' \
    'TasksMax=90%' \
    > /etc/systemd/system/user-.slice.d/worklane.conf \
 && printf '%s\n' \
    '[Manager]' \
    'DefaultTasksMax=90%' \
    > /etc/systemd/user.conf.d/worklane.conf \
 && printf '%s\n' \
    '[Service]' \
    'ExecStart=' \
    'ExecStart=/usr/bin/install -d -m 0700 -o %i -g %i /run/user/%i' \
    'ExecStop=' \
    'ExecStop=/usr/bin/rm -rf /run/user/%i' \
    > /etc/systemd/system/user-runtime-dir@.service.d/worklane.conf \
 && printf '%s\n' \
    '[Unit]' \
    'Description=Worklane Herdr session server' \
    '' \
    '[Service]' \
    'Type=simple' \
    'WorkingDirectory=%h' \
    'ExecStartPre=/usr/bin/mkdir -p %h/.config/herdr' \
    'ExecStart=/usr/local/bin/herdr --session ${WORKLANE_SESSION} server' \
    'ExecStartPost=-%h/.local/share/worklane/bin/worklane-herdr-reconcile' \
    'Restart=on-failure' \
    'RestartSec=1s' \
    'KillMode=mixed' \
    > /usr/lib/systemd/user/worklane-herdr.service \
 && ln -sfn ../worklane-herdr.service /usr/lib/systemd/user/default.target.wants/worklane-herdr.service \
 && printf '%s\n' \
    '[Unit]' \
    'Description=Worklane Git diff pane manager' \
    'After=worklane-herdr.service' \
    'Requires=worklane-herdr.service' \
    'ConditionPathIsExecutable=%h/.local/share/worklane/bin/worklane-git-diff-pane-manager' \
    '' \
    '[Service]' \
    'Type=simple' \
    'WorkingDirectory=%h' \
    'ExecStart=%h/.local/share/worklane/bin/worklane-git-diff-pane-manager' \
    'Restart=on-failure' \
    'RestartSec=2s' \
    'KillMode=mixed' \
    > /usr/lib/systemd/user/worklane-git-diff-pane-manager.service
COPY --chmod=755 assets/worklane-pi-agent /usr/local/bin/worklane-pi-agent
COPY --chmod=755 assets/worklane-pi-install /usr/local/lib/worklane/worklane-pi-install
COPY assets/worklane-runtime/collab.ts assets/worklane-runtime/p0.ts assets/worklane-runtime/complain.ts /usr/local/lib/worklane/runtime/
COPY --chmod=755 assets/worklane-runtime/runtime.py /usr/local/bin/worklane-collab
COPY assets/worklane-runtime/worklane-collab.service assets/worklane-runtime/worklane-goal-runner.service assets/worklane-runtime/worklane-goal-runner.timer /usr/lib/systemd/user/
RUN ln -sfn ../worklane-collab.service /usr/lib/systemd/user/default.target.wants/worklane-collab.service \
 && install -d /usr/lib/systemd/user/timers.target.wants \
 && ln -sfn ../worklane-goal-runner.timer /usr/lib/systemd/user/timers.target.wants/worklane-goal-runner.timer
COPY --chmod=755 assets/worklane-ide assets/worklane-appearance /usr/local/lib/worklane/
COPY assets/worklane-ide-init.lua /usr/local/lib/worklane/ide-init.lua
USER ${USERNAME}
WORKDIR /home/${USERNAME}
ENV container=podman
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
