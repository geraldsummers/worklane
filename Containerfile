FROM debian:trixie-slim
# worklane-standard-containerfile
ARG CODEX_VERSION=latest
ARG TYPESCRIPT_VERSION=7.0.2
ARG PRETTIER_VERSION=3.9.5
ARG RUST_VERSION=1.85.1
ARG RUSTUP_INIT_SHA256=6c30b75a75b28a96fd913a037c8581b580080b6ee9b8169a3c0feb1af7fe8caf
ARG HERDR_VERSION=0.7.4
ARG HERDR_SHA256=bc0fc02d4ba500f9cac2353a43e67fe036785ecca6eb55378e050fac3c103059
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
    tzdata valgrind vim wget xvfb xauth xz-utils yq zip zsh chromium kotlin \
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
 && python3 --version && pip3 --version && node --version && npm --version \
 && tsc --version && prettier --version && rustc --version && cargo --version && rustfmt --version \
 && java --version && kotlinc -version
RUN groupadd --gid "${USER_GID}" "${USERNAME}" && useradd --uid "${USER_UID}" --gid "${USER_GID}" -m -s /usr/bin/zsh "${USERNAME}"
USER ${USERNAME}
WORKDIR /home/${USERNAME}
CMD ["sleep", "infinity"]
