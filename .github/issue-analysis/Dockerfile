FROM node:22-bookworm-slim

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global @qoder-ai/qodercli@1.1.77 \
    && qodercli --version \
    && npm cache clean --force

USER node
WORKDIR /source
