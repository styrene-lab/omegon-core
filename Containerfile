FROM debian:bookworm-slim

# Minimal deps: curl for install, ca-certificates for HTTPS
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Install omegon-agent from GitHub Releases
RUN curl -fsSL https://raw.githubusercontent.com/styrene-lab/omegon-core/main/install.sh | sh

# Verify binary exists and runs
RUN omegon-agent --help

# Auth credentials mount point
# Mount at runtime: -v ~/.pi/agent:/root/.pi/agent:ro
VOLUME /root/.pi/agent

WORKDIR /workspace

ENTRYPOINT ["omegon-agent"]
