FROM ghcr.io/protectskills/claude-skill-sandbox:lite@sha256:f6f993df6f9c3f8b608c50548da598f4be862e020f204afa3d653701e95e07bc
USER root
ENV PATH=/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
COPY claude-observed /usr/local/bin/claude
COPY observe.py /opt/poc/observe.py
COPY scanner-source/agent_sec_cli /opt/poc/scanner-source/agent_sec_cli
ENV PYTHONPATH=/opt/poc/scanner-source
RUN python -m pip install --no-cache-dir openai==2.54.0 pydantic==2.13.5 PyYAML==6.0.3 tqdm==4.70.0 \
    && chmod 755 /usr/local/bin/claude && mkdir -p /app/work \
    && /usr/bin/claude --version && strace -V
RUN apt-get update && apt-get install -y --no-install-recommends binutils && apt-get clean
WORKDIR /app
CMD ["sleep", "infinity"]
