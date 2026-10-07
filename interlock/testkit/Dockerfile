# A Linux machine for interlock's remaining tests, with every tool they use.
#
# interlock runs on Linux only: it reads /proc for process ancestry and
# containment. This image gives a Mac or Windows machine the environment the
# build was tested in. From the interlock directory:
#
#   docker build -t interlock-testkit -f testkit/Dockerfile .
#   docker run -it --name interlock-testkit interlock-testkit
#
# Inside, `testkit/run.sh doctor` says what each test still needs. Leave with
# `exit`, come back with `docker start -ai interlock-testkit`, and copy the
# results out with `docker cp interlock-testkit:/home/tester/interlock/testkit-results .`
#
# Versions are pinned to the ones the build was verified with. Override with
# --build-arg, for example COPILOT_VERSION=latest if GitHub refuses 1.0.91.

ARG BASE=ubuntu:24.04
FROM ${BASE}

ARG RUST_VERSION=1.97.0
ARG NODE_VERSION=22.22.0
ARG COPILOT_VERSION=1.0.91
ARG CLAUDE_VERSION=2.1.292
ARG GH_VERSION=2.89.0

ENV DEBIAN_FRONTEND=noninteractive
# A download that fails must fail the build, not pipe nothing into tar.
SHELL ["/bin/bash", "-euo", "pipefail", "-c"]

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential ca-certificates curl git golang-go jq pkg-config python3 unzip xz-utils zip \
 && rm -rf /var/lib/apt/lists/*

# The GitHub CLI, from its release on GitHub, checked against the release's checksums.
RUN arch="$(dpkg --print-architecture)" && cd /tmp \
 && base="https://github.com/cli/cli/releases/download/v${GH_VERSION}" \
 && curl -fsSLO "${base}/gh_${GH_VERSION}_linux_${arch}.tar.gz" \
 && curl -fsSL "${base}/gh_${GH_VERSION}_checksums.txt" | grep " gh_${GH_VERSION}_linux_${arch}.tar.gz\$" | sha256sum -c - \
 && tar -xzf "gh_${GH_VERSION}_linux_${arch}.tar.gz" \
 && install -m 0755 "gh_${GH_VERSION}_linux_${arch}/bin/gh" /usr/local/bin/gh \
 && rm -rf /tmp/gh_*

# Node, checked against its published checksums, then the two hosts:
# Copilot CLI (first class) and Claude Code.
RUN arch="$(dpkg --print-architecture | sed 's/amd64/x64/')" && cd /tmp \
 && file="node-v${NODE_VERSION}-linux-${arch}.tar.xz" \
 && curl -fsSLO "https://nodejs.org/dist/v${NODE_VERSION}/${file}" \
 && curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/SHASUMS256.txt" | grep " ${file}\$" | sha256sum -c - \
 && tar -xJf "${file}" -C /usr/local --strip-components=1 \
 && rm "${file}" \
 && npm install -g "@github/copilot@${COPILOT_VERSION}" "@anthropic-ai/claude-code@${CLAUDE_VERSION}" \
 && npm cache clean --force

RUN useradd -m -s /bin/bash tester
USER tester
ENV PATH=/home/tester/.cargo/bin:/home/tester/interlock/target/release:$PATH

# Rust through rustup-init from static.rust-lang.org, checked against its checksum.
RUN cd /tmp \
 && base="https://static.rust-lang.org/rustup/dist/$(uname -m)-unknown-linux-gnu" \
 && curl -fsSLO "${base}/rustup-init" \
 && curl -fsSL "${base}/rustup-init.sha256" | sha256sum -c - \
 && chmod +x rustup-init \
 && ./rustup-init -y --no-modify-path --profile minimal --default-toolchain "${RUST_VERSION}" -c clippy -c rustfmt \
 && rm rustup-init \
 && git config --global user.name tester \
 && git config --global user.email tester@interlock.invalid \
 && git config --global init.defaultBranch main

WORKDIR /home/tester/interlock
COPY --chown=tester:tester . .
RUN cargo build --release --locked -p interlock-cli

CMD ["bash"]
