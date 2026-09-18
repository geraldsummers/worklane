FROM debian:trixie-slim
# worklane-standard-containerfile
ARG CODEX_VERSION=latest
ARG TYPESCRIPT_VERSION=7.0.2
ARG PRETTIER_VERSION=3.9.5
ARG RUST_VERSION=1.85.1
ARG RUSTUP_INIT_SHA256=7d0ea0f8eba7fa1ebfe998091cd7ec4501e33ec5ca6b884eb4d894d7da5170af
ARG HERDR_VERSION=0.8.2
ARG HERDR_SHA256=976150a14d490c94b243ea2e1a7eb2dfb67f12e36b182db90936f6728e6aecf4
ARG USERNAME=dev
ARG USER_UID=1000
ARG USER_GID=1000
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
    netcat-openbsd ninja-build nodejs npm openssh-client patch \
    pkg-config procps psmisc python-is-python3 python3 python3-pip python3-venv systemd systemd-sysv \
    graphicsmagick pipx python3-tomlkit ripgrep rsync shellcheck socat sqlite3 strace tar tree unzip \
    tzdata valgrind vim wget xvfb xauth xz-utils yq zip zsh chromium \
 && rm -rf /var/lib/apt/lists/* \
 && ln -s /usr/bin/batcat /usr/local/bin/bat \
 && ln -s /usr/bin/fdfind /usr/local/bin/fd \
 && env NPM_CONFIG_PREFIX=/usr/local npm install -g "@openai/codex@${CODEX_VERSION}" "typescript@${TYPESCRIPT_VERSION}" "prettier@${PRETTIER_VERSION}" \
 && mv /usr/local/bin/codex /usr/local/bin/codex-real \
 && printf '%s\n' \
    '#!/bin/sh' \
    'for arg do' \
    '  case "$arg" in' \
    '    --yolo|--dangerously-bypass-approvals-and-sandbox) exec /usr/local/bin/codex-real "$@" ;;' \
    '  esac' \
    'done' \
    'exec /usr/local/bin/codex-real -a never -s danger-full-access "$@"' \
    > /usr/local/bin/codex \
 && chmod +x /usr/local/bin/codex \
 && mkdir -p /etc/codex \
 && printf '%s\n' 'approval_policy = "never"' 'sandbox_mode = "danger-full-access"' > /etc/codex/config.toml \
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
USER ${USERNAME}
WORKDIR /home/${USERNAME}
ENV container=podman
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
