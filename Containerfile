FROM debian:trixie-slim
ARG CODEX_VERSION=latest
ARG USERNAME=dev
ARG USER_UID=1000
ARG USER_GID=1000
ENV DEBIAN_FRONTEND=noninteractive SHELL=/usr/bin/zsh LANG=C.UTF-8 LC_ALL=C.UTF-8 \
    RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl git gnupg less openssh-client procps sudo zsh \
    nodejs npm python3 python3-pip python3-venv python-is-python3 pipx \
    openjdk-21-jdk kotlin build-essential pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/* \
 && npm install -g "@openai/codex@${CODEX_VERSION}" typescript prettier \
 && curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o /tmp/rustup-init.sh \
 && sh /tmp/rustup-init.sh -y --profile minimal \
 && rm -f /tmp/rustup-init.sh \
 && rustup toolchain install stable \
 && rustup default stable \
 && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
 && echo "deb [signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
 && apt-get update && apt-get install -y --no-install-recommends gh && rm -rf /var/lib/apt/lists/* \
 && curl -fsSL https://herdr.dev/install.sh -o /tmp/herdr-install.sh \
 && HERDR_INSTALL_DIR=/usr/local/bin sh /tmp/herdr-install.sh \
 && command -v herdr \
 && herdr --version \
 && rm -f /tmp/herdr-install.sh \
 && python3 --version && pip3 --version && node --version && npm --version \
 && tsc --version && prettier --version && rustc --version && cargo --version \
 && java --version && kotlinc -version
RUN groupadd --gid "${USER_GID}" "${USERNAME}" && useradd --uid "${USER_UID}" --gid "${USER_GID}" -m -s /usr/bin/zsh "${USERNAME}" \
 && echo "${USERNAME} ALL=(ALL) NOPASSWD:ALL" > "/etc/sudoers.d/${USERNAME}" \
 && chown -R "${USERNAME}:${USERNAME}" /usr/local/rustup /usr/local/cargo
USER ${USERNAME}
WORKDIR /home/${USERNAME}/workspace
CMD ["sleep", "infinity"]
