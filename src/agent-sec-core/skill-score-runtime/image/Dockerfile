ARG BASE_IMAGE=ghcr.io/protectskills/claude-skill-sandbox:lite@sha256:f6f993df6f9c3f8b608c50548da598f4be862e020f204afa3d653701e95e07bc
FROM ${BASE_IMAGE}
USER root
ENV PATH=/usr/local/bin:${PATH}
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      bash ca-certificates curl git strace binutils python3 python3-pip \
    && (command -v npm >/dev/null || apt-get install -y --no-install-recommends nodejs npm) \
    && rm -rf /var/lib/apt/lists/* \
    && (command -v python || ln -s "$(command -v python3)" /usr/local/bin/python) \
    && npm install --prefix /opt/asc/claude --no-audit --no-fund @anthropic-ai/claude-code@2.1.143
COPY claude-observed /usr/local/bin/claude
COPY observe.py /opt/poc/observe.py
COPY scanner-source/agent_sec_cli /opt/poc/scanner-source/agent_sec_cli
ENV PYTHONPATH=/opt/poc/scanner-source${PYTHONPATH:+:${PYTHONPATH}}
RUN python -m pip install --no-cache-dir openai==2.54.0 pydantic==2.13.5 PyYAML==6.0.3 tqdm==4.70.0 \
    && chmod 755 /usr/local/bin/claude \
    && mkdir -p /app/work /root/.local/bin "${HOME:-/root}/.local/bin" \
    && ln -sf /usr/local/bin/claude /root/.local/bin/claude \
    && ln -sf /usr/local/bin/claude "${HOME:-/root}/.local/bin/claude" \
    && /opt/asc/claude/node_modules/.bin/claude --version && strace -V
WORKDIR /app
# The business base may start FC services; Harbor owns this container lifecycle.
ENTRYPOINT []
CMD ["sleep", "infinity"]
