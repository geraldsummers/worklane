FROM debian:trixie-slim
ARG CODEX_VERSION=latest
ARG USERNAME=dev
ARG USER_UID=1000
ARG USER_GID=1000
ENV DEBIAN_FRONTEND=noninteractive SHELL=/usr/bin/zsh LANG=C.UTF-8 LC_ALL=C.UTF-8
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl git gnupg less openssh-client procps sudo zsh nodejs npm && rm -rf /var/lib/apt/lists/* \
 && npm install -g "@openai/codex@${CODEX_VERSION}" \
 && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
 && echo "deb [signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
 && apt-get update && apt-get install -y --no-install-recommends gh && rm -rf /var/lib/apt/lists/* \
 && curl -fsSL https://herdr.dev/install.sh -o /tmp/herdr-install.sh \
 && HERDR_INSTALL_DIR=/usr/local/bin sh /tmp/herdr-install.sh \
 && command -v herdr \
 && herdr --version \
 && rm -f /tmp/herdr-install.sh
RUN groupadd --gid "${USER_GID}" "${USERNAME}" && useradd --uid "${USER_UID}" --gid "${USER_GID}" -m -s /usr/bin/zsh "${USERNAME}" && echo "${USERNAME} ALL=(ALL) NOPASSWD:ALL" > "/etc/sudoers.d/${USERNAME}"
USER ${USERNAME}
WORKDIR /home/${USERNAME}/workspace
CMD ["sleep", "infinity"]
