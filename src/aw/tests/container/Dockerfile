# syntax=docker/dockerfile:1
FROM ghcr.io/astral-sh/uv:0.11.13@sha256:841c8e6fe30a8b07b4478d12d0c608cba6de66102d29d65d1cc423af86051563 AS uv
FROM node:24.16.0-bookworm-slim@sha256:2c87ef9bd3c6a3bd4b472b4bec2ce9d16354b0c574f736c476489d09f560a203 AS node
COPY pi/package*.json /opt/pi/
RUN npm ci --prefix /opt/pi --ignore-scripts --no-audit --no-fund

FROM ubuntu:24.04@sha256:68ee22004a9109b44973313b02ff7b1de250f1de0f5ecb628d73f579aaff33ac

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates git libatomic1 libssl3t64 libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /opt/pi/ /opt/pi/
ENV UV_PYTHON_INSTALL_DIR=/opt/python PYTHONDONTWRITEBYTECODE=1
COPY requirements.txt /opt/requirements.txt
COPY python-version.txt /opt/python-version.txt
RUN --mount=from=uv,source=/uv,target=/usr/local/bin/uv \
    uv venv --python "$(cat /opt/python-version.txt)" /opt/venv \
    && uv pip install --python /opt/venv/bin/python -r /opt/requirements.txt \
    && rm -rf /root/.cache/uv
COPY payload/ /opt/aw/bin/
COPY source/ /opt/project/src/aw/
RUN useradd --uid 10001 --create-home aw \
    && mkdir /evidence /work \
    && chmod -R a+rX /opt/project /opt/aw \
    && chown aw:aw /evidence /work /opt/pi \
    && chmod 700 /evidence /work /home/aw
ENV PATH=/opt/venv/bin:/opt/aw/bin:/usr/local/bin:/usr/bin:/bin \
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
USER aw
WORKDIR /opt/project/src/aw
ENTRYPOINT ["/opt/venv/bin/python", "-B", "tests/resident_image.py"]
