FROM debian:trixie-slim
ARG CODEX_VERSION=latest
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
    dbus-x11 diffutils dnsutils fd-find file findutils fonts-liberation \
    fonts-noto-color-emoji fzf gawk gdb git git-lfs gnupg golang-go jq less \
    libffi-dev libsqlite3-dev libssl-dev lsof make man-db nano ncdu \
    netcat-openbsd ninja-build nodejs npm openjdk-21-jdk openssh-client patch \
    pkg-config procps psmisc python-is-python3 python3 python3-pip python3-venv \
    pipx ripgrep rsync shellcheck socat sqlite3 strace tar tree unzip \
    valgrind vim wget xvfb xauth xz-utils yq zip zsh chromium kotlin \
 && rm -rf /var/lib/apt/lists/* \
 && ln -s /usr/bin/batcat /usr/local/bin/bat \
 && ln -s /usr/bin/fdfind /usr/local/bin/fd \
 && env NPM_CONFIG_PREFIX=/usr/local npm install -g "@openai/codex@${CODEX_VERSION}" typescript prettier \
 && mv /usr/local/bin/codex /usr/local/bin/codex-real \
 && printf '%s\n' \
    '#!/bin/sh' \
    'for arg do' \
    '  [ "$arg" = "--yolo" ] && exec /usr/local/bin/codex-real "$@"' \
    'done' \
    'exec /usr/local/bin/codex-real --yolo "$@"' \
    > /usr/local/bin/codex \
 && chmod +x /usr/local/bin/codex \
 && mkdir -p /etc/codex \
 && printf '%s\n' 'approval_policy = "never"' 'sandbox_mode = "danger-full-access"' > /etc/codex/config.toml \
 && curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o /tmp/rustup-init.sh \
 && env CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup sh /tmp/rustup-init.sh -y --profile minimal \
 && rm -f /tmp/rustup-init.sh \
 && env CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup rustup toolchain install stable \
 && env CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup rustup component add rustfmt \
 && env CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup rustup default stable \
 && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
 && echo "deb [signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
 && apt-get update && apt-get install -y --no-install-recommends gh && rm -rf /var/lib/apt/lists/* \
 && curl -fsSL https://herdr.dev/install.sh -o /tmp/herdr-install.sh \
 && HERDR_INSTALL_DIR=/usr/local/bin sh /tmp/herdr-install.sh \
 && command -v herdr \
 && herdr --version \
 && rm -f /tmp/herdr-install.sh \
 && python3 --version && pip3 --version && node --version && npm --version \
 && tsc --version && prettier --version && rustc --version && cargo --version && rustfmt --version \
 && java --version && kotlinc -version
RUN groupadd --gid "${USER_GID}" "${USERNAME}" && useradd --uid "${USER_UID}" --gid "${USER_GID}" -m -s /usr/bin/zsh "${USERNAME}"
USER ${USERNAME}
WORKDIR /home/${USERNAME}
CMD ["sleep", "infinity"]
